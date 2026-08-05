// NR: zero-fill one storage texture. Used to reset the à trous accumulator's
// `acc_a` slot to zero at the start of every evaluate (see `nr_node.rs`'s
// module doc for why only that one ping-pong slot needs it). Kept as its own
// trivial compute pass — rather than a render-pass `LoadOp::Clear`, which
// would require adding `RENDER_ATTACHMENT` usage to an otherwise
// compute-only texture and mixing render/compute passes on one resource in
// the same encoder — so the whole node stays uniformly compute, the common
// (well-exercised) case throughout this crate.
@group(0) @binding(0) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(dst));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(0.0));
}
