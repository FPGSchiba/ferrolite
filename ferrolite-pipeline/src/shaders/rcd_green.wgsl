// RCD pass 1: Hamilton-Adams directional green interpolation.
// Input: normalized single-channel CFA (storage buffer, row-major w*h).
// Output: full green plane (storage buffer, row-major w*h).
// Mirrors ferrolite_decode::rcd::interpolate_green exactly (RGGB, phase (0,0)).
struct Params { width: u32, height: u32, pad0: u32, pad1: u32, wb: vec4<f32> };
@group(0) @binding(0) var<storage, read> cfa: array<f32>;
@group(0) @binding(1) var<storage, read_write> green: array<f32>;
@group(0) @binding(2) var<uniform> p: Params;

fn s(x: i32, y: i32) -> f32 {
    let w = i32(p.width);
    let h = i32(p.height);
    let xc = clamp(x, 0, w - 1);
    let yc = clamp(y, 0, h - 1);
    return cfa[u32(yc) * p.width + u32(xc)];
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height) { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);
    let idx = gid.y * p.width + gid.x;
    let pos = (gid.y % 2u) * 2u + (gid.x % 2u);
    if (pos == 1u || pos == 2u) {
        green[idx] = s(x, y); // G site: measured
        return;
    }
    let center = s(x, y);
    let gh = abs(s(x - 1, y) - s(x + 1, y)) + abs(2.0 * center - s(x - 2, y) - s(x + 2, y));
    let gv = abs(s(x, y - 1) - s(x, y + 1)) + abs(2.0 * center - s(x, y - 2) - s(x, y + 2));
    let gh_est = 0.5 * (s(x - 1, y) + s(x + 1, y)) + 0.25 * (2.0 * center - s(x - 2, y) - s(x + 2, y));
    let gv_est = 0.5 * (s(x, y - 1) + s(x, y + 1)) + 0.25 * (2.0 * center - s(x, y - 2) - s(x, y + 2));
    var g: f32;
    if (gh < gv) { g = gh_est; } else if (gv < gh) { g = gv_est; } else { g = 0.5 * (gh_est + gv_est); }
    green[idx] = g;
}
