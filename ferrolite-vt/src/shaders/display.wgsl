struct Transform {
    zoom: f32,
    _pad0: f32,
    pan: vec2<f32>,
    viewport: vec2<f32>,
    image: vec2<f32>,
};
@group(0) @binding(0) var img_tex: texture_2d<f32>;
@group(0) @binding(1) var img_samp: sampler;
@group(0) @binding(2) var<uniform> xf: Transform;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) screen_uv: vec2<f32>, // 0..1 across the viewport
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Full-screen triangle.
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var out: VsOut;
    let xy = p[vid];
    out.pos = vec4(xy, 0.0, 1.0);
    out.screen_uv = (xy * 0.5 + vec2(0.5, 0.5)) * vec2(1.0, -1.0) + vec2(0.0, 1.0);
    return out;
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Screen pixel -> image pixel: center the image, apply pan, divide by zoom.
    let screen_px = in.screen_uv * xf.viewport;
    let center = xf.image * 0.5 + xf.pan;
    let img_px = center + (screen_px - xf.viewport * 0.5) / xf.zoom;
    let uv = img_px / xf.image;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return vec4(0.05, 0.05, 0.05, 1.0);
    }
    let lin = textureSampleLevel(img_tex, img_samp, uv, 0.0).rgb;
    return vec4(linear_to_srgb(lin), 1.0);
}

// ---- Rung 2: tiled mip pyramid + per-fragment LOD selection ----

@group(0) @binding(3) var tiles: texture_2d_array<f32>;
@group(0) @binding(4) var<storage, read> slots: array<u32>;

// Sentinel slot index meaning "tile not resident" (matches Rust `pool::NOT_RESIDENT`).
const NOT_RESIDENT: u32 = 0xFFFFFFFFu;

struct TileMeta {
    level_count: u32,
    _pad: vec3<u32>,
    // Per-level packing for up to 8 levels: x = cols (tiles per row), y = flat slot offset.
    levels: array<vec4<u32>, 8>,
};
@group(0) @binding(5) var<uniform> tmeta: TileMeta;

fn pick_lod(img_px: vec2<f32>) -> u32 {
    let dx = length(dpdx(img_px));
    let dy = length(dpdy(img_px));
    let d = max(max(dx, dy), 1.0);
    return min(u32(max(log2(d), 0.0)), tmeta.level_count - 1u);
}

@fragment
fn fs_tiled(in: VsOut) -> @location(0) vec4<f32> {
    let screen_px = in.screen_uv * xf.viewport;
    let center = xf.image * 0.5 + xf.pan;
    let img_px = center + (screen_px - xf.viewport * 0.5) / xf.zoom;
    if (img_px.x < 0.0 || img_px.x >= xf.image.x || img_px.y < 0.0 || img_px.y >= xf.image.y) {
        return vec4(0.05, 0.05, 0.05, 1.0);
    }
    let lod = pick_lod(img_px);
    // Coarse-LOD fallback: start at the picked LOD and walk up to coarser levels
    // until a resident tile is found. The in-tile UV uses `lvl` (the resolved
    // level), never the originally picked `lod`.
    var lvl = lod;
    var slot = NOT_RESIDENT;
    var lod_px = vec2<f32>(0.0, 0.0);
    var tx = 0u;
    var ty = 0u;
    loop {
        lod_px = img_px / f32(1u << lvl);
        tx = u32(lod_px.x) / 256u;
        ty = u32(lod_px.y) / 256u;
        let cols = tmeta.levels[lvl].x;
        let offset = tmeta.levels[lvl].y;
        let cand = slots[offset + ty * cols + tx];
        if (cand != NOT_RESIDENT) {
            slot = cand;
            break;
        }
        if (lvl + 1u >= tmeta.level_count) {
            break;
        }
        lvl = lvl + 1u;
    }
    if (slot == NOT_RESIDENT) {
        return vec4(0.05, 0.05, 0.05, 1.0);
    }
    let in_tile = (lod_px - vec2(f32(tx * 256u), f32(ty * 256u))) / 256.0;
    let lin = textureSampleLevel(tiles, img_samp, in_tile, slot, 0.0).rgb;
    return vec4(linear_to_srgb(lin), 1.0);
}

// ---- Rung 4: page-table indirection + GPU feedback pass ----
//
// The slot for a virtual tile is resolved from a page-table texture (`page_table`)
// instead of the `slots` storage buffer. The shader ALSO marks the tile it wanted
// (at the picked LOD) into the `feedback` storage buffer, which the CPU reads back
// one frame later to drive streaming (GPU-truth visibility).

@group(0) @binding(6) var page_table: texture_2d<u32>;
@group(0) @binding(7) var<storage, read_write> feedback: array<atomic<u32>>;

// Flat index of (lvl, tx, ty) into the page-table / feedback arrays.
fn flat_at(lvl: u32, tx: u32, ty: u32) -> u32 {
    let cols = tmeta.levels[lvl].x;
    let offset = tmeta.levels[lvl].y;
    return offset + ty * cols + tx;
}

// Resolve a virtual tile's slot from the page table.
fn page_table_slot(flat: u32) -> u32 {
    return textureLoad(page_table, vec2<i32>(i32(flat), 0), 0).r;
}

@fragment
fn fs_sparse(in: VsOut) -> @location(0) vec4<f32> {
    let screen_px = in.screen_uv * xf.viewport;
    let center = xf.image * 0.5 + xf.pan;
    let img_px = center + (screen_px - xf.viewport * 0.5) / xf.zoom;
    if (img_px.x < 0.0 || img_px.x >= xf.image.x || img_px.y < 0.0 || img_px.y >= xf.image.y) {
        return vec4(0.05, 0.05, 0.05, 1.0);
    }
    let lod = pick_lod(img_px);

    // Mark the desired tile (at the PICKED lod) as needed — GPU-truth feedback of
    // what the shader wanted, independent of which level the fallback resolves to.
    let picked_px = img_px / f32(1u << lod);
    let picked_tx = u32(picked_px.x) / 256u;
    let picked_ty = u32(picked_px.y) / 256u;
    let picked_flat = flat_at(lod, picked_tx, picked_ty);
    atomicOr(&feedback[picked_flat], 1u);

    // Coarse-LOD fallback: climb to coarser levels until a resident tile is found,
    // resolving the slot from the page table. In-tile UV uses the resolved `lvl`.
    var lvl = lod;
    var slot = NOT_RESIDENT;
    var lod_px = vec2<f32>(0.0, 0.0);
    var tx = 0u;
    var ty = 0u;
    loop {
        lod_px = img_px / f32(1u << lvl);
        tx = u32(lod_px.x) / 256u;
        ty = u32(lod_px.y) / 256u;
        let cand = page_table_slot(flat_at(lvl, tx, ty));
        if (cand != NOT_RESIDENT) {
            slot = cand;
            break;
        }
        if (lvl + 1u >= tmeta.level_count) {
            break;
        }
        lvl = lvl + 1u;
    }
    if (slot == NOT_RESIDENT) {
        return vec4(0.05, 0.05, 0.05, 1.0);
    }
    let in_tile = (lod_px - vec2(f32(tx * 256u), f32(ty * 256u))) / 256.0;
    let lin = textureSampleLevel(tiles, img_samp, in_tile, slot, 0.0).rgb;
    return vec4(linear_to_srgb(lin), 1.0);
}
