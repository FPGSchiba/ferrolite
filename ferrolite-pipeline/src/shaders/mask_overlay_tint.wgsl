// Fullscreen-triangle overlay tint: sample the R32F coverage by framebuffer
// position and output PREMULTIPLIED red. Rendered into a linear Rgba8Unorm
// target whose dims equal the coverage dims (so textureLoad by pixel position is
// 1:1). An Rgba8UnormSrgb view of that target is handed to egui. Mirrors the
// pure `overlay_tint` in mask_overlay.rs.

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Robust fullscreen triangle:
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    let pos = uv * 2.0 - vec2<f32>(1.0, 1.0);
    return vec4<f32>(pos, 0.0, 1.0);
}

@group(0) @binding(0) var coverage: texture_2d<f32>;

struct TintParams { strength: f32, _pad0: f32, _pad1: f32, _pad2: f32 };
@group(0) @binding(1) var<uniform> params: TintParams;

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = vec2<i32>(i32(frag.x), i32(frag.y));
    let c = textureLoad(coverage, px, 0).r;
    let a = clamp(c, 0.0, 1.0) * params.strength;
    return vec4<f32>(a, 0.0, 0.0, a); // premultiplied red
}
