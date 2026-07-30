// Separable sharpen pass 1/3: HORIZONTAL half of the box blur that feeds the
// unsharp-mask apply pass. Replaces sharpen.wgsl's O((2r+1)^2) fused 2D loop
// with two O(2r+1) 1D passes (this + sharpen_box_v.wgsl) — see
// sharpen_node.rs's module doc for why the clamped-edge box mean is separable
// (H-then-V, each normalized by `2r+1`, equals the fused 2D mean normalized by
// `(2r+1)^2`) PROVIDED each pass clamps only its own axis, exactly mirrored
// here (x clamped within the row; y unchanged).
//
// `p.amount` is unused here (kept only so this struct's layout matches
// `SharpenUniform` byte-for-byte with the apply pass's uniform — the node
// writes ONE buffer and binds it to all three passes). The node's `evaluate`
// never dispatches this shader when `amount == 0 || radius <= 0` (identity
// passthrough — see `sharpen_node.rs`), so `p.radius` is always >= 1 here.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P { amount: f32, radius: i32, pad0: f32, pad1: f32 };
@group(0) @binding(2) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(src));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let r = p.radius;

    var sum = vec3<f32>(0.0);
    for (var dx = -r; dx <= r; dx = dx + 1) {
        let qx = clamp(xy.x + dx, 0, dims.x - 1);
        sum = sum + textureLoad(src, vec2<i32>(qx, xy.y), 0).rgb;
    }
    let n = f32(2 * r + 1);
    textureStore(dst, xy, vec4<f32>(sum / n, 0.0));
}
