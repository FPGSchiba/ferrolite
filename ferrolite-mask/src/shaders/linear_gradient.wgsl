// Linear-gradient mask: mask = clamped scalar projection of the pixel's
// normalized position onto the start->end axis. 0 at (and before) `start`,
// 1 at (and after) `end`, linear between (the feathered band = |end - start|).
// Analytic per pixel -> zero halo, tiles cleanly in source space.
@group(0) @binding(0) var out_tex: texture_storage_2d<r32float, write>;
struct P { start: vec2<f32>, end: vec2<f32> };
@group(0) @binding(1) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(out_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + vec2<f32>(0.5, 0.5))
        / vec2<f32>(f32(dims.x), f32(dims.y));
    let axis = p.end - p.start;
    let len2 = dot(axis, axis);
    var t = 0.0;
    if (len2 > 1e-12) {
        t = clamp(dot(uv - p.start, axis) / len2, 0.0, 1.0);
    }
    textureStore(out_tex, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(t, 0.0, 0.0, 1.0));
}
