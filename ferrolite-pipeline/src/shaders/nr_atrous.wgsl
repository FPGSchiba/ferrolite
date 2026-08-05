// NR: ONE à trous level, fused. Computes the 2D B3-spline [1,4,6,4,1]/16 outer
// product at hole spacing `p.spacing` (= 2^level), derives this level's detail
// coefficient, soft-shrinks it, and accumulates — all in one pass.
//
// Deliberately NOT separable H-then-V (spec §3.3): at a fixed 5 taps, separable
// is 10 taps vs 25 but costs an extra full-res texture AND an extra full-res
// round-trip, and these passes are bandwidth-bound. `nr.rs`'s
// `separable_b3spline_equals_direct` proves this 2D form equals the separable
// reference, so the shipped pass has a verified oracle.
//
// At level 0 the node binds the ORIGINAL working-space image as both `src` and
// `approx`, and this shader converts RGB->YCbCr on load (the `p.level == 0`
// branch) so no separate conversion pass or texture is needed.
// `approx` serves BOTH roles — the convolution input and the detail base —
// because the reference is `detail = approx - b3_spline_2d(approx)`. One binding,
// not two.
@group(0) @binding(0) var approx: texture_2d<f32>;
@group(0) @binding(1) var acc_in: texture_2d<f32>;
@group(0) @binding(2) var dst_next: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var dst_acc: texture_storage_2d<rgba16float, write>;
// `active` is a reserved WGSL keyword (naga rejects it as a field name), so
// this field is named `nr_active` instead — the WGSL field NAME need not
// match the Rust `NrUniform` field name, only the byte layout (order/size),
// since the buffer is uploaded as raw bytes via `bytemuck`. Unused by this
// shader (kept only for layout parity with `NrUniform`).
// `canvas` is the inclusive [min_x, min_y, max_x, max_y] of the TRUE CANVAS in
// this buffer's coords — see `NrUniform::canvas`. Taps clamp to it, NOT to
// `dims`, so the tiled tier (whose buffer extends past the canvas into a
// geometry-head-replicated halo) reproduces the whole-image tier's boundary
// exactly. For the whole-image tier it IS `[0, 0, dims-1]`.
struct P { thresholds: array<vec4<f32>, 2>, nr_active: i32, spacing: i32, level: i32, pad: f32, canvas: vec4<i32> };
@group(0) @binding(4) var<uniform> p: P;

fn to_ycbcr(rgb: vec3<f32>) -> vec3<f32> {
    let y = 0.2126 * rgb.r + 0.7152 * rgb.g + 0.0722 * rgb.b;
    return vec3<f32>(y, rgb.b - y, rgb.r - y);
}

// Load a texel, converting RGB->YCbCr at level 0 only (level 0's input is the
// original working-space image; later levels are already YCbCr).
fn fetch(xy: vec2<i32>, lvl: i32) -> vec3<f32> {
    let c = textureLoad(approx, xy, 0).rgb;
    if (lvl == 0) { return to_ycbcr(c); }
    return c;
}

// Soft shrinkage — mirrors `nr::shrink` exactly. Hard thresholding produces the
// "plastic" look and is deliberately not used.
fn soft_shrink(d: f32, t: f32) -> f32 {
    let m = abs(d) - t;
    if (m <= 0.0) { return 0.0; }
    return select(m, -m, d < 0.0);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(approx));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let s = p.spacing;
    // Dynamic (non-constant-index) array access requires an actual memory
    // location in this naga version — neither a module-scope `const` array
    // nor a function-local `let` (a pure value, no address) may be indexed by
    // a runtime value (`ky`/`kx` below); only a `var` (function-scope
    // storage) can. Confirmed by compiling both rejected forms.
    var b: array<f32, 5> = array<f32, 5>(0.0625, 0.25, 0.375, 0.25, 0.0625);

    // Fused 2D B3-spline: the separable kernel's outer product, clamping both
    // axes to the CANVAS (clamp DUPLICATES the border texel, matching `nr.rs`'s
    // `clamp_idx`). Clamping to `p.canvas` rather than to `dims` is what keeps
    // the tiled tier in step with the whole-image tier — a tap that would land
    // in the haloed buffer's replicated out-of-canvas region instead reads the
    // canvas border texel of THIS level's approx, which is precisely what the
    // whole-image tier reads. See the `canvas` note on `struct P`.
    var next = vec3<f32>(0.0);
    for (var ky = 0; ky < 5; ky = ky + 1) {
        let dy = (ky - 2) * s;
        let yy = clamp(xy.y + dy, p.canvas.y, p.canvas.w);
        for (var kx = 0; kx < 5; kx = kx + 1) {
            let dx = (kx - 2) * s;
            let xx = clamp(xy.x + dx, p.canvas.x, p.canvas.z);
            next = next + b[ky] * b[kx] * fetch(vec2<i32>(xx, yy), p.level);
        }
    }

    let a_raw = textureLoad(approx, xy, 0);
    var a = a_raw.rgb;
    if (p.level == 0) { a = to_ycbcr(a); }

    let detail = a - next;
    let t_luma = p.thresholds[0].x;
    let t_chroma = p.thresholds[0].y;
    let shrunk = vec3<f32>(
        soft_shrink(detail.r, t_luma),
        soft_shrink(detail.g, t_chroma),
        soft_shrink(detail.b, t_chroma),
    );

    // Level 0 SEEDS the accumulator (writes `shrunk` alone) instead of adding
    // to `acc_in`; every later level accumulates. This is what makes a separate
    // zero-fill pass unnecessary: the reference (`nr::atrous_shrink_reference`)
    // starts `acc` at zero, so "level 0 = shrunk" is exactly equivalent to
    // "level 0 = 0 + shrunk" — but it derives the zero from the LEVEL INDEX,
    // which is always correct, rather than from the accumulator texture's
    // residual content, which had to be re-zeroed every evaluate and silently
    // corrupted output when it wasn't (see the retired `nr_clear.wgsl`).
    // `acc_in` is still BOUND at level 0 (the bind group's shape is fixed) but
    // is never read there, so its content is irrelevant.
    var acc_prev = vec3<f32>(0.0);
    if (p.level != 0) { acc_prev = textureLoad(acc_in, xy, 0).rgb; }

    textureStore(dst_next, xy, vec4<f32>(next, a_raw.a));
    textureStore(dst_acc, xy, vec4<f32>(acc_prev + shrunk, a_raw.a));
}
