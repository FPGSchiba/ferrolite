// Separable sharpen pass 2/3: VERTICAL half of the box blur, paired with
// `sharpen_box_h.wgsl`. Reads that pass's horizontal-blur output and clamps
// only the y axis (x already resolved by the H pass), so the composed H-then-V
// result equals the fused 2D box mean within float order — see
// `sharpen_node.rs`'s module doc.
//
// `p.amount`/`p.detail`/`p.masking` are unused here (see `sharpen_box_h.wgsl`'s
// doc for why the uniform layout still carries them).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P { amount: f32, radius: i32, detail: f32, masking: f32 };
@group(0) @binding(2) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(src));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let r = p.radius;

    var sum = vec3<f32>(0.0);
    for (var dy = -r; dy <= r; dy = dy + 1) {
        let qy = clamp(xy.y + dy, 0, dims.y - 1);
        sum = sum + textureLoad(src, vec2<i32>(xy.x, qy), 0).rgb;
    }
    let n = f32(2 * r + 1);
    textureStore(dst, xy, vec4<f32>(sum / n, 0.0));
}
