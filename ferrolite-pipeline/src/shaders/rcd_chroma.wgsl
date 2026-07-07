// RCD pass 2: constant-hue (colour-difference) red/blue, then per-channel WB.
// Inputs: normalized CFA + interpolated green (storage buffers).
// Output: rgba16float storage texture (WB'd, UNCLAMPED — carries >1/negatives).
// Mirrors ferrolite_decode::rcd::reconstruct_rgb + the caller's WB multiply.
struct Params { width: u32, height: u32, pad0: u32, pad1: u32, wb: vec4<f32> };
@group(0) @binding(0) var<storage, read> cfa: array<f32>;
@group(0) @binding(1) var<storage, read> green: array<f32>;
@group(0) @binding(2) var out_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var<uniform> p: Params;

fn cs(x: i32, y: i32) -> f32 {
    let w = i32(p.width);
    let h = i32(p.height);
    return cfa[u32(clamp(y, 0, h - 1)) * p.width + u32(clamp(x, 0, w - 1))];
}
fn gs(x: i32, y: i32) -> f32 {
    let w = i32(p.width);
    let h = i32(p.height);
    return green[u32(clamp(y, 0, h - 1)) * p.width + u32(clamp(x, 0, w - 1))];
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height) { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);
    let pos = (gid.y % 2u) * 2u + (gid.x % 2u);
    let g_here = green[gid.y * p.width + gid.x];
    var r: f32;
    var g: f32;
    var b: f32;
    if (pos == 0u) {
        // R site: R measured; B from 4 diagonal B neighbours.
        r = cs(x, y);
        g = g_here;
        b = g_here + 0.25 * ((cs(x - 1, y - 1) - gs(x - 1, y - 1))
            + (cs(x + 1, y - 1) - gs(x + 1, y - 1))
            + (cs(x - 1, y + 1) - gs(x - 1, y + 1))
            + (cs(x + 1, y + 1) - gs(x + 1, y + 1)));
    } else if (pos == 3u) {
        // B site: B measured; R from 4 diagonal R neighbours.
        b = cs(x, y);
        g = g_here;
        r = g_here + 0.25 * ((cs(x - 1, y - 1) - gs(x - 1, y - 1))
            + (cs(x + 1, y - 1) - gs(x + 1, y - 1))
            + (cs(x - 1, y + 1) - gs(x - 1, y + 1))
            + (cs(x + 1, y + 1) - gs(x + 1, y + 1)));
    } else if (pos == 1u) {
        // G site (even row, odd col): R horizontal, B vertical.
        g = cs(x, y);
        r = g + 0.5 * ((cs(x - 1, y) - gs(x - 1, y)) + (cs(x + 1, y) - gs(x + 1, y)));
        b = g + 0.5 * ((cs(x, y - 1) - gs(x, y - 1)) + (cs(x, y + 1) - gs(x, y + 1)));
    } else {
        // pos == 2: G site (odd row, even col): B horizontal, R vertical.
        g = cs(x, y);
        b = g + 0.5 * ((cs(x - 1, y) - gs(x - 1, y)) + (cs(x + 1, y) - gs(x + 1, y)));
        r = g + 0.5 * ((cs(x, y - 1) - gs(x, y - 1)) + (cs(x, y + 1) - gs(x, y + 1)));
    }
    textureStore(out_tex, vec2<i32>(x, y), vec4<f32>(r * p.wb.x, g * p.wb.y, b * p.wb.z, 1.0));
}
