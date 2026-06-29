// Fullscreen gradient — proves egui hosts a live wgpu render pass (Gate 0).
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // Fullscreen triangle.
    let x = f32(i32(i) / 2) * 4.0 - 1.0;
    let y = f32(i32(i) & 1) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    // Steel-blue accent gradient (#6d97b5 family) so a glance confirms it rendered.
    let uv = frag.xy / 720.0;
    return vec4<f32>(0.10 + 0.30 * uv.x, 0.20 + 0.35 * uv.y, 0.45, 1.0);
}
