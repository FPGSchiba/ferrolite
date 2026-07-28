// Transmission mip-chain downsample (LOD fix): builds one mip level of the
// shared dehaze transmission map from the level above it with a 2x2 box
// average. Run once per transmission rebuild (level 1..N in a loop, each pass
// reading the previous level, writing the next), NOT per frame — the graph's
// dirty-caching keeps the whole transmission (and thus its mips) off the
// amount-drag path, same as the guided-filter passes it follows.
//
// Why mips: the tiled recovery samples this whole-image map at each output
// pixel's source UV. When the displayed LOD is COARSER than the ~1536px map
// (zoomed out past fit), one output pixel covers many transmission texels; a
// single point/bilinear sample of the base level undersamples the sharp
// guided-refined transmission edges and rings. `dehaze_recovery.wgsl` picks a
// band-limited LOD (`transmission_sample_lod`) instead, which needs this chain.
//
// `textureLoad` (no sampler): each destination texel maps to an exact 2x2 block
// of the source level, clamped at the source edge for odd dimensions.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dst_dims = vec2<i32>(textureDimensions(dst));
    if (i32(gid.x) >= dst_dims.x || i32(gid.y) >= dst_dims.y) {
        return;
    }
    let src_dims = vec2<i32>(textureDimensions(src));
    let x0 = i32(gid.x) * 2;
    let y0 = i32(gid.y) * 2;
    let x1 = min(x0 + 1, src_dims.x - 1);
    let y1 = min(y0 + 1, src_dims.y - 1);
    let c = (textureLoad(src, vec2<i32>(x0, y0), 0)
        + textureLoad(src, vec2<i32>(x1, y0), 0)
        + textureLoad(src, vec2<i32>(x0, y1), 0)
        + textureLoad(src, vec2<i32>(x1, y1), 0))
        * 0.25;
    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), c);
}
