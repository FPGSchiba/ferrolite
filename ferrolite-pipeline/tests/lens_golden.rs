//! GPU goldens for the lens-correction chain (Spec 4.4 Task 10 / U6).
//!
//! These run on a real GPU (auto-skip when `GpuContext::headless()` is `None`).
//! Unlike the `golden.rs` PNG goldens, the load-bearing lens golden here is
//! reference-FREE: `corrected_render_matches_cpu_reference` compares the GPU
//! render against a CPU reimplementation of `geometry.wgsl`'s warp sampling that
//! reuses the SAME manual-bilinear + `i/(n-1)` node convention. This settles the
//! U5 "convention risk": if the shader ever sampled the bake with a different
//! convention (texel centers `(i+0.5)/n` instead of nodes `i/(n-1)`, a
//! transposed grid, etc.) the two would diverge and this test fails. No PNG
//! fixture to drift; the bake + shader must agree by construction.

mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_lens::{load_bundled, LensDb, LensMatch, LensQuery, WarpGrid, GRID_N, VIGNETTE_LEN};
use ferrolite_pipeline::{
    lens_uniform, vignette_amount, Correction, EditPipeline, GpuPyramidSource, LensCorrection, Op,
    OpStack, TileEditPipeline, VignetteTexture, WarpGridTexture,
};
use std::sync::Arc;

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// The bundled distorting lens used by the U2 lens-crate tests: a real Lensfun
/// distortion + TCA calibration at focal 24 (wide end distorts most).
fn fixture_lens() -> Option<(ferrolite_lens::LensfunDb, LensMatch)> {
    let db = load_bundled().ok()?;
    let q = LensQuery {
        camera_make: "Canon".into(),
        camera_model: "Canon EOS 5D Mark III".into(),
        lens_model: Some("Canon EF 24-70mm f/2.8L II USM".into()),
        focal_len: 24.0,
        aperture: 8.0,
    };
    let m = db.match_lens(&q)?;
    Some((db, m))
}

/// A `LensCorrection` op with distortion + TCA + vignetting enabled at full
/// strength, matching the fixture lens capture context.
fn corrected_lens_op() -> LensCorrection {
    LensCorrection {
        lens_id: Some("Canon EF 24-70mm f/2.8L II USM".into()),
        focal_len: 24.0,
        aperture: 8.0,
        crop_factor: 1.0,
        distortion: Correction {
            enabled: true,
            amount: 1.0,
        },
        tca: Correction {
            enabled: true,
            amount: 1.0,
        },
        vignetting: Correction {
            enabled: true,
            amount: 1.0,
        },
    }
}

/// A smooth two-axis color ramp (distinct per-channel gradients so a per-channel
/// TCA shift is visible, and smooth so bilinear resampling error stays small).
fn smooth_source(w: u32, h: u32) -> LinearRgbaF32 {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / (w - 1).max(1) as f32;
            let fy = y as f32 / (h - 1).max(1) as f32;
            // R rises in x, B rises in y, G is a diagonal blend — all channels
            // carry a spatial gradient so a per-channel warp is detectable.
            px.extend_from_slice(&[fx, 0.5 * (fx + fy), fy, 1.0]);
        }
    }
    LinearRgbaF32::new(w, h, px).expect("smooth source length")
}

// ---------------------------------------------------------------------------
// CPU reference for `geometry.wgsl` (use_warp == 1), identity geometry.
// ---------------------------------------------------------------------------

/// Round-trip a source through f16 (matching `upload_source`'s GPU storage) into
/// a flat `[r,g,b,a]` f32 buffer we can sample on the CPU.
fn f16_roundtrip(src: &LinearRgbaF32) -> Vec<f32> {
    src.pixels
        .iter()
        .map(|&v| half::f16::from_f32(v).to_f32())
        .collect()
}

/// Bilinear sample of the f16-round-tripped source at normalized `uv`, matching
/// the shader's `textureSampleLevel` with a Linear filter + ClampToEdge address
/// mode: texel centers at `(i+0.5)/dim`, so texel space is `uv*dim - 0.5`.
fn sample_bilinear_clamp(px: &[f32], w: u32, h: u32, uv: (f32, f32)) -> [f32; 4] {
    let fx = uv.0 * w as f32 - 0.5;
    let fy = uv.1 * h as f32 - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = fx - x0;
    let ty = fy - y0;
    let clampi = |v: f32, hi: u32| (v as i64).clamp(0, hi as i64 - 1) as usize;
    let x0i = clampi(x0, w);
    let x1i = clampi(x0 + 1.0, w);
    let y0i = clampi(y0, h);
    let y1i = clampi(y0 + 1.0, h);
    let fetch = |xi: usize, yi: usize| {
        let o = (yi * w as usize + xi) * 4;
        [px[o], px[o + 1], px[o + 2], px[o + 3]]
    };
    let p00 = fetch(x0i, y0i);
    let p10 = fetch(x1i, y0i);
    let p01 = fetch(x0i, y1i);
    let p11 = fetch(x1i, y1i);
    let mut out = [0.0f32; 4];
    for c in 0..4 {
        let top = p00[c] + (p10[c] - p00[c]) * tx;
        let bot = p01[c] + (p11[c] - p01[c]) * tx;
        out[c] = top + (bot - top) * ty;
    }
    out
}

/// Manual bilinear fetch of the warp grid at normalized `base_uv`, EXACTLY as
/// `geometry.wgsl::warp_sample` does it: node `i` sits at `i/(n-1)`, invert with
/// `g = base_uv*(n-1)`, clamp to `[0, n-1]`, lerp the 4 neighboring nodes.
/// Returns `(r_uv, g_uv, b_uv)` — the per-channel distorted source coords.
fn warp_sample_cpu(grid: &WarpGrid, base_uv: (f32, f32)) -> ([f32; 2], [f32; 2], [f32; 2]) {
    let n = grid.n;
    let nm1 = (n as f32 - 1.0).max(0.0);
    let gx = (base_uv.0 * nm1).clamp(0.0, nm1);
    let gy = (base_uv.1 * nm1).clamp(0.0, nm1);
    let gx0 = gx.floor();
    let gy0 = gy.floor();
    let gx1 = (gx0 + 1.0).min(nm1);
    let gy1 = (gy0 + 1.0).min(nm1);
    let fx = gx - gx0;
    let fy = gy - gy0;
    let node = |xi: f32, yi: f32| -> [f32; 6] {
        let idx = (yi as u32 * n + xi as u32) as usize;
        grid.coords[idx]
    };
    let c00 = node(gx0, gy0);
    let c10 = node(gx1, gy0);
    let c01 = node(gx0, gy1);
    let c11 = node(gx1, gy1);
    let mut lerped = [0.0f32; 6];
    for k in 0..6 {
        let top = c00[k] + (c10[k] - c00[k]) * fx;
        let bot = c01[k] + (c11[k] - c01[k]) * fx;
        lerped[k] = top + (bot - top) * fy;
    }
    (
        [lerped[0], lerped[1]],
        [lerped[2], lerped[3]],
        [lerped[4], lerped[5]],
    )
}

/// Full CPU reference for the corrected geometry pass at identity geometry:
/// for each output pixel, compute `base_uv`, sample the warp, compose the
/// per-channel finals with the given amounts, and bilinear-sample the source.
/// Mirrors `geometry.wgsl` line for line. Returns a linear RGBA f32 buffer.
fn cpu_corrected_reference(
    src: &LinearRgbaF32,
    grid: &WarpGrid,
    dist_amount: f32,
    tca_amount: f32,
) -> Vec<f32> {
    let (w, h) = (src.width, src.height);
    let px = f16_roundtrip(src);
    let mut out = vec![0.0f32; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            // Identity geometry: po = gid + 0.5; src = po; base_uv = po/dims.
            let base_uv = ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
            let (r_full, g_uv, b_full) = warp_sample_cpu(grid, base_uv);
            let mix2 = |a: [f32; 2], b: [f32; 2], t: f32| {
                [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
            };
            let base = [base_uv.0, base_uv.1];
            let r_uv = mix2(g_uv, r_full, tca_amount);
            let bch_uv = mix2(g_uv, b_full, tca_amount);
            let r_final = mix2(base, r_uv, dist_amount);
            let g_final = mix2(base, g_uv, dist_amount);
            let b_final = mix2(base, bch_uv, dist_amount);
            let r = sample_bilinear_clamp(&px, w, h, (r_final[0], r_final[1]))[0];
            let g_sample = sample_bilinear_clamp(&px, w, h, (g_final[0], g_final[1]));
            let b = sample_bilinear_clamp(&px, w, h, (b_final[0], b_final[1]))[2];
            let o = ((y * w + x) * 4) as usize;
            out[o] = r;
            out[o + 1] = g_sample[1];
            out[o + 2] = b;
            out[o + 3] = g_sample[3];
        }
    }
    out
}

fn max_channel_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max)
}

// ---------------------------------------------------------------------------
// Golden 1: corrections-off ≡ geometry-only (identity guarantee, byte-identical).
// ---------------------------------------------------------------------------

#[test]
fn corrections_off_equals_geometry_only() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let (w, h) = (64u32, 48u32);
    let src = smooth_source(w, h);

    // A LensCorrection op with EVERYTHING disabled must be a no-op: use_warp=0,
    // all amounts 0, vig_amount 0 → the shader takes the byte-identical path.
    let disabled = LensCorrection {
        distortion: Correction {
            enabled: false,
            amount: 1.0,
        },
        tca: Correction {
            enabled: false,
            amount: 1.0,
        },
        vignetting: Correction {
            enabled: false,
            amount: 1.0,
        },
        ..corrected_lens_op()
    };
    let with_op = OpStack::default().set_op(Op::LensCorrection(disabled));
    let without_op = OpStack::default();

    // No warp/vignette bake is bound (app never bakes for a disabled op), so the
    // pipeline binds identity defaults regardless.
    let mut a = EditPipeline::new(ctx.clone(), &src, with_op, IDENTITY);
    let mut b = EditPipeline::new(ctx.clone(), &src, without_op, IDENTITY);
    let ra = a.render_to_image();
    let rb = b.render_to_image();
    assert_eq!(
        common::max_abs_diff(&ra, &rb),
        0,
        "a disabled LensCorrection op must render byte-identical to no op"
    );
}

// ---------------------------------------------------------------------------
// Golden 2: corrected render matches the independent CPU reference.
// ---------------------------------------------------------------------------

/// GPU-linear tolerance. Absorbs f16 source storage + hardware-bilinear rounding
/// vs. the CPU reference's f32 bilinear. The shader and the CPU ref use the same
/// convention, so the residual is purely numeric, not structural.
const CORRECTED_TOL: f32 = 0.02;

#[test]
fn corrected_render_matches_cpu_reference() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let Some((db, m)) = fixture_lens() else {
        eprintln!("bundled lens db unavailable; skipping");
        return;
    };
    let ctx = Arc::new(ctx);
    let grid = db
        .bake_geometry(&m, 24.0, GRID_N)
        .expect("fixture lens has a distortion model");

    // Sanity: the fixture genuinely carries a per-channel TCA split, so the R/B
    // channels MUST diverge from green somewhere (asserted structurally below).
    let (w, h) = (96u32, 72u32);
    let src = smooth_source(w, h);

    // GPU: whole-image EditPipeline with only the lens op, warp grid + amounts
    // bound (dist=1, tca=1). Identity geometry, so the geometry pass base_uv is
    // exactly (gid+0.5)/dims — matching the CPU reference.
    let stack = OpStack::default().set_op(Op::LensCorrection(corrected_lens_op()));
    let mut pipe = EditPipeline::new(ctx.clone(), &src, stack, IDENTITY);
    pipe.set_warp(WarpGridTexture::upload(&ctx, &grid));
    pipe.set_lens_uniform(lens_uniform(Some(&corrected_lens_op()), true));
    // Vignetting is a separate pass; leave it identity here so this golden
    // isolates the geometry warp convention (vignetting has its own golden).
    let gpu_lin = common::read_image_linear(&ctx, &pipe.evaluate());

    // CPU reference: same grid, same convention, dist=1, tca=1.
    let cpu_ref = cpu_corrected_reference(&src, &grid, 1.0, 1.0);

    let diff = max_channel_diff(&gpu_lin, &cpu_ref);
    eprintln!("corrected-render GPU-vs-CPU max linear diff = {diff}");
    assert!(
        diff <= CORRECTED_TOL,
        "GPU corrected render diverged from the CPU reference (diff {diff}) — \
         shader/bake convention mismatch?"
    );

    // Structural TCA check: the baked grid must carry a genuine per-channel
    // coordinate split (R/B destination coords differ from green), and that split
    // must survive into the rendered image. First assert the grid split directly
    // (the load-bearing proof that TCA is baked), then that it produces a
    // measurable rendered difference vs a TCA-off warp of the same grid.
    let grid_tca_split = grid
        .coords
        .iter()
        .map(|c| {
            // max |R - G| and |B - G| over the two coordinate axes.
            let rg = (c[0] - c[2]).abs().max((c[1] - c[3]).abs());
            let bg = (c[4] - c[2]).abs().max((c[5] - c[3]).abs());
            rg.max(bg)
        })
        .fold(0.0f32, f32::max);
    eprintln!("grid TCA coord split (normalized) = {grid_tca_split}");
    assert!(
        grid_tca_split > 1e-6,
        "fixture lens grid must carry a real per-channel TCA split"
    );

    let cpu_no_tca = cpu_corrected_reference(&src, &grid, 1.0, 0.0);
    let tca_effect = max_channel_diff(&cpu_ref, &cpu_no_tca);
    eprintln!("rendered TCA effect (tca=1 vs tca=0) = {tca_effect}");
    assert!(
        tca_effect > 1e-6,
        "the baked TCA split must produce a measurable rendered R/B divergence"
    );

    // And the GPU render tracks the tca=1 ref more closely than the tca=0 ref,
    // proving the GPU actually applied TCA (not just distortion).
    let diff_to_no_tca = max_channel_diff(&gpu_lin, &cpu_no_tca);
    assert!(
        diff < diff_to_no_tca,
        "GPU render must match the TCA-on reference better than the TCA-off one"
    );
}

// ---------------------------------------------------------------------------
// Golden 2b: vignetting darkens/brightens per the baked gain LUT.
// ---------------------------------------------------------------------------

#[test]
fn vignetting_applies_radial_gain() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let Some((db, m)) = fixture_lens() else {
        eprintln!("bundled lens db unavailable; skipping");
        return;
    };
    let ctx = Arc::new(ctx);
    // Wide open vignettes most (matches the U2 vignetting test).
    let Some(vmap) = db.bake_vignetting(&m, 24.0, 2.8, VIGNETTE_LEN) else {
        eprintln!("no vignetting calibration; skipping");
        return;
    };
    let (w, h) = (96u32, 72u32);
    // Flat mid-grey so the only spatial variation is the vignette gain.
    let flat = LinearRgbaF32::new(
        w,
        h,
        vec![0.5f32; (w * h * 4) as usize]
            .iter()
            .enumerate()
            .map(|(i, &v)| if i % 4 == 3 { 1.0 } else { v })
            .collect(),
    )
    .unwrap();

    let mut lc = corrected_lens_op();
    lc.aperture = 2.8;
    // Isolate vignetting: disable the geometry warp so only the radial gain acts.
    lc.distortion.enabled = false;
    lc.tca.enabled = false;
    let stack = OpStack::default().set_op(Op::LensCorrection(lc.clone()));
    let mut pipe = EditPipeline::new(ctx.clone(), &flat, stack, IDENTITY);
    pipe.set_vignette(VignetteTexture::upload(&ctx, &vmap));
    pipe.set_vig_amount(vignette_amount(Some(&lc)));
    let out = common::read_image_linear(&ctx, &pipe.evaluate());

    // Center pixel gain ≈ radial[0]; a corner ≈ radial[last]. The correction
    // brightens the corners (gain grows outward), so corner > center.
    let center_idx = (((h / 2) * w + w / 2) * 4) as usize;
    let corner_idx = 0usize; // top-left corner
    let center_r = out[center_idx];
    let corner_r = out[corner_idx];
    eprintln!("vignette center R = {center_r}, corner R = {corner_r}");
    assert!(
        corner_r > center_r + 1e-3,
        "vignetting correction must brighten the corners relative to the center"
    );
    // Center stays near the un-vignetted mid-grey (gain[0] ≈ 1.0).
    assert!(
        (center_r - 0.5).abs() < 0.05,
        "center gain should be ~1.0 (near-identity at image center)"
    );
}

// ---------------------------------------------------------------------------
// Golden 3: tile-seam — the corrected tiled producer matches the whole image.
// ---------------------------------------------------------------------------

/// Display-linear seam tolerance (mirrors `golden.rs::SEAM_TOL`); absorbs f16 +
/// the head resample across the tile boundary.
const SEAM_TOL: f32 = 0.02;

#[test]
fn corrected_tiles_match_whole_image_at_seam() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let Some((db, m)) = fixture_lens() else {
        eprintln!("bundled lens db unavailable; skipping");
        return;
    };
    let ctx = Arc::new(ctx);
    let grid = db
        .bake_geometry(&m, 24.0, GRID_N)
        .expect("fixture lens has a distortion model");

    // Multi-tile image: 300x200 → 2x1 tiles at LOD 0 (seam at x = 256).
    let (iw, ih) = (300u32, 200u32);
    let src = smooth_source(iw, ih);
    let lc = corrected_lens_op();
    let stack = OpStack::default().set_op(Op::LensCorrection(lc.clone()));

    // Whole-image reference through EditPipeline with the lens bound.
    let mut whole = EditPipeline::new(ctx.clone(), &src, stack.clone(), IDENTITY);
    whole.set_warp(WarpGridTexture::upload(&ctx, &grid));
    whole.set_lens_uniform(lens_uniform(Some(&lc), true));
    let whole_lin = common::read_image_linear(&ctx, &whole.evaluate());

    // Per-tile producer with the SAME bake; the lens halo is folded into the
    // haloed tile extent at construction.
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    let mut tep = TileEditPipeline::new(ctx.clone(), pyramid, stack, IDENTITY, Some(&grid), None);
    assert!(
        tep.halo() > 0,
        "a distorting lens must bake a non-zero halo into the tile producer"
    );

    use ferrolite_image::{TileCoord, TILE_SIZE};
    let mut max_diff = 0.0f32;
    let mut seam_max = 0.0f32;
    for tx in 0..2u32 {
        let tile = tep.produce_tile(TileCoord {
            lod: 0,
            x: tx,
            y: 0,
        });
        let tile_lin = common::read_tile_linear(&ctx, &tile);
        for ly in 0..TILE_SIZE {
            for lx in 0..TILE_SIZE {
                let gx = tx * TILE_SIZE + lx;
                let gy = ly;
                if gx >= iw || gy >= ih {
                    continue; // out-of-image tile padding
                }
                let ti = ((ly * TILE_SIZE + lx) * 4) as usize;
                let wi = ((gy * iw + gx) * 4) as usize;
                for c in 0..3 {
                    let d = (tile_lin[ti + c] - whole_lin[wi + c]).abs();
                    max_diff = max_diff.max(d);
                    // Track the seam column (±2 px around x = 256) separately.
                    if gx.abs_diff(TILE_SIZE) <= 2 {
                        seam_max = seam_max.max(d);
                    }
                }
            }
        }
    }
    eprintln!("corrected tile max diff = {max_diff}, seam-column max = {seam_max}");
    assert!(
        max_diff <= SEAM_TOL,
        "per-tile corrected render diverged from whole-image (diff {max_diff}) — lens halo broken?"
    );
}
