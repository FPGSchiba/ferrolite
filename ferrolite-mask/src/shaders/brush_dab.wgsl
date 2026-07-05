// Dab-stamping rasterizer. Reads the current accumulator (in_tex), stamps a batch
// of dabs (in normalized source coords) in order, writes the new accumulator
// (out_tex). in_tex and out_tex share dims. Output texel gid maps to a level
// pixel `origin + gid`, then to normalized uv `(pixel + 0.5) / level_dims` — so a
// haloed tile (origin < 0 possible) evaluates identical uv to the whole image.
struct Dab {
    center: vec2<f32>,
    radius: f32,
    hardness: f32,
    flow: f32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
};

struct Params {
    origin: vec2<i32>,
    level_dims: vec2<u32>,
    dab_count: u32,
    erase: u32,
    pad0: u32,
    pad1: u32,
};

@group(0) @binding(0) var in_tex: texture_2d<f32>;
@group(0) @binding(1) var out_tex: texture_storage_2d<r32float, write>;
@group(0) @binding(2) var<uniform> p: Params;
@group(0) @binding(3) var<storage, read> dabs: array<Dab>;

fn dab_alpha(dist: f32, radius: f32, hardness: f32, flow: f32) -> f32 {
    if (radius <= 0.0) { return 0.0; }
    let t = dist / radius;
    let core = clamp(hardness, 0.0, 1.0);
    var ring = 0.0;
    if (t < core) {
        ring = 1.0;
    } else if (t >= 1.0) {
        ring = 0.0;
    } else {
        ring = 1.0 - smoothstep(0.0, 1.0, (t - core) / (1.0 - core));
    }
    return ring * clamp(flow, 0.0, 1.0);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(out_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }

    let px = vec2<f32>(
        f32(i32(gid.x) + p.origin.x),
        f32(i32(gid.y) + p.origin.y),
    );
    let uv = (px + vec2<f32>(0.5, 0.5))
        / vec2<f32>(f32(p.level_dims.x), f32(p.level_dims.y));

    var acc = textureLoad(in_tex, vec2<i32>(i32(gid.x), i32(gid.y)), 0).r;
    for (var i = 0u; i < p.dab_count; i = i + 1u) {
        let d = dabs[i];
        let dist = distance(uv, d.center);
        let a = dab_alpha(dist, d.radius, d.hardness, d.flow);
        if (p.erase == 1u) {
            acc = acc * (1.0 - a);
        } else {
            acc = acc + (1.0 - acc) * a;
        }
    }
    textureStore(out_tex, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(acc, 0.0, 0.0, 1.0));
}
