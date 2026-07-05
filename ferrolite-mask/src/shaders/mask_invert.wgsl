// Mask invert: out = 1 - in, per pixel. Applied once after folding when a
// MaskDefinition has invert = true.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var out_tex: texture_storage_2d<r32float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(out_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let v = textureLoad(src, xy, 0).r;
    textureStore(out_tex, xy, vec4<f32>(1.0 - v, 0.0, 0.0, 1.0));
}
