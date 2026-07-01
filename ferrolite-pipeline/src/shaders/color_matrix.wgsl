// Camera->working color transform: multiply linear RGB by a 3x3 matrix. Point op.
// Bindings match PointOpNode: 0 = src texture, 1 = dst storage, 2 = matrix uniform.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct M { m: mat3x3<f32> };
@group(0) @binding(2) var<uniform> cm: M;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    textureStore(dst, xy, vec4<f32>(cm.m * c.rgb, c.a));
}
