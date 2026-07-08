//! Pure Dark Channel Prior (He et al.) dehaze math — no GPU. The GPU pass
//! (`shaders/dehaze.wgsl`) mirrors `dehaze_recover` exactly; the atmospheric
//! light `A` is a whole-image estimate computed once (design §5.3) and handed to
//! every tile as a uniform. `dehaze_recover` is the reusable transform the future
//! per-mask path (design §7) will call unchanged (design §2.5).

use crate::op::Dehaze;
use ferrolite_image::LinearRgbaF32;

/// Default dark-channel min-filter patch radius (px), seeded for a brand-new op
/// by the Effects tab. The radius is USER-EXPOSED (`Dehaze::radius`); this is only
/// the initial value. Design §5.2 suggests 7–15.
pub const DEHAZE_DEFAULT_RADIUS: u32 = 8;
/// Safety cap on the dehaze patch radius (px): bounds the min-filter loop and
/// prevents a u32→i32 wrap to negative (mirrors `MAX_SHARPEN_RADIUS`).
pub const MAX_DEHAZE_RADIUS: u32 = 64;
/// Haze-retention factor ω (design §5.2, step 3): keep a little haze for realism.
pub(crate) const DEHAZE_OMEGA: f32 = 0.95;
/// Transmission floor t₀ (design §5.2, step 4): avoids divide-by-~0 noise blow-up.
const DEHAZE_T0: f32 = 0.1;
/// The identity-safe atmospheric light used before a real estimate is available
/// (e.g. `TileEditPipeline` before `set_dehaze_atmos`, or a no-dehaze export).
/// With `amount == 0` the recovery is identity regardless of `A`, so this is only
/// ever a placeholder for the no-op case.
pub const DEHAZE_ATMOS_NEUTRAL: [f32; 3] = [1.0, 1.0, 1.0];
/// Floor each `A` channel to this to keep the `I/A` and `/max(t,t0)` divisions finite.
const DEHAZE_ATMOS_MIN: f32 = 1e-3;
/// Cap on pixels scanned by `estimate_atmospheric_light` (it subsamples above
/// this). Bounds the CPU cost to sub-millisecond regardless of image size so it
/// is safe to run at pipeline construction (CLAUDE.md rule 1 — no multi-ms UI work).
const MAX_ATMOS_SAMPLES: usize = 262_144;

/// Per-pixel dark channel: the min of the three linear channels.
fn dark_channel(rgb: [f32; 3]) -> f32 {
    rgb[0].min(rgb[1]).min(rgb[2])
}

/// Whole-image atmospheric-light estimate `A` (design §5.3): the mean RGB of the
/// brightest ~0.1% of pixels by per-pixel dark channel. Subsamples to at most
/// `MAX_ATMOS_SAMPLES` pixels so the cost is bounded (safe at construction, off
/// the per-frame path — CLAUDE.md rule 1). Each channel is floored to
/// `DEHAZE_ATMOS_MIN` so downstream divisions stay finite. Deterministic (fixed
/// stride), so the preview and tiled tiers computing it from the same image agree.
pub fn estimate_atmospheric_light(img: &LinearRgbaF32) -> [f32; 3] {
    let n = (img.width as usize) * (img.height as usize);
    if n == 0 {
        return DEHAZE_ATMOS_NEUTRAL;
    }
    let stride = n.div_ceil(MAX_ATMOS_SAMPLES).max(1);
    // (dark_channel, [r,g,b]) for each sampled pixel.
    let mut samples: Vec<(f32, [f32; 3])> = Vec::new();
    let mut i = 0usize;
    while i < n {
        let base = i * 4;
        let rgb = [img.pixels[base], img.pixels[base + 1], img.pixels[base + 2]];
        samples.push((dark_channel(rgb), rgb));
        i += stride;
    }
    // Brightest 0.1% by dark channel (at least one). `select_nth_unstable_by`
    // partitions the top `keep` elements to the front in O(n) rather than a
    // full O(n log n) sort — the RESULT is identical (same top-k set; the mean
    // taken below is order-independent), just cheaper, matching the cap
    // comment above ("bounds the CPU cost ... regardless of image size").
    let keep = (samples.len() / 1000).max(1);
    samples.select_nth_unstable_by(keep - 1, |a, b| {
        b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut acc = [0.0f32; 3];
    for (_, rgb) in samples.iter().take(keep) {
        for c in 0..3 {
            acc[c] += rgb[c];
        }
    }
    let inv = 1.0 / keep as f32;
    [
        (acc[0] * inv).max(DEHAZE_ATMOS_MIN),
        (acc[1] * inv).max(DEHAZE_ATMOS_MIN),
        (acc[2] * inv).max(DEHAZE_ATMOS_MIN),
    ]
}

/// Per-pixel DCP recovery (design §5.2) — the reusable transform (design §2.5)
/// the WGSL kernel mirrors exactly. `dark` is the patch dark channel of the
/// NORMALIZED image `I/A` in `[0,1]` (computed by the caller/shader over the halo
/// patch). Transmission `t = 1 - ω·dark`, floored at `t0` for recovery:
///   remove-haze  J_c = (I_c - A_c)/max(t, t0) + A_c
///   add-haze  hazed_c = A_c + (I_c - A_c)·t          (symmetric, toward A)
/// `amount >= 0` blends I→J by `amount`; `amount < 0` blends I→hazed by `|amount|`.
/// Not clamped (out-of-range values pass through; display clamps later).
pub fn dehaze_recover(px: [f32; 3], dark: f32, a: [f32; 3], amount: f32) -> [f32; 3] {
    let t = (1.0 - DEHAZE_OMEGA * dark).clamp(0.0, 1.0);
    let te = t.max(DEHAZE_T0);
    let mut out = [0.0f32; 3];
    for c in 0..3 {
        let j = (px[c] - a[c]) / te + a[c];
        let hazed = a[c] + (px[c] - a[c]) * t;
        out[c] = if amount >= 0.0 {
            px[c] + amount * (j - px[c])
        } else {
            px[c] + (-amount) * (hazed - px[c])
        };
    }
    out
}

/// Guided-filter regularization ε (design step 5): larger = smoother/less edge-aware.
pub const DEHAZE_GUIDED_EPS: f32 = 1e-3;

/// Guided-filter window radius as a function of the patch radius (one knob).
/// Must be a large enough multiple of `r` that the guided window straddles the
/// luma edge across the FULL width of the block-min dilation halo (width ≈ `r`),
/// otherwise the far end of the halo band never sees the edge and the filter
/// blurs it instead of removing it (see `guided_refinement_removes_most_of_the_block_min_halo`).
pub fn guided_radius(r: u32) -> u32 {
    r.saturating_mul(3)
}

/// Separable clamp-to-edge min over a `(2r+1)²` window: horizontal min pass then
/// vertical min pass. Equals the naïve patch min but O(2r) per pixel, not O(r²).
pub(crate) fn min_filter_separable(plane: &[f32], w: usize, h: usize, r: i32) -> Vec<f32> {
    let idx = |x: i32, y: i32| -> usize {
        (y.clamp(0, h as i32 - 1) as usize) * w + x.clamp(0, w as i32 - 1) as usize
    };
    let mut horiz = vec![0.0f32; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut m = f32::INFINITY;
            for dx in -r..=r {
                m = m.min(plane[idx(x + dx, y)]);
            }
            horiz[idx(x, y)] = m;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut m = f32::INFINITY;
            for dy in -r..=r {
                m = m.min(horiz[idx(x, y + dy)]);
            }
            out[idx(x, y)] = m;
        }
    }
    out
}

/// Separable clamp-to-edge normalized box average of radius `r` (H then V).
pub(crate) fn box_blur_separable(plane: &[f32], w: usize, h: usize, r: i32) -> Vec<f32> {
    let idx = |x: i32, y: i32| -> usize {
        (y.clamp(0, h as i32 - 1) as usize) * w + x.clamp(0, w as i32 - 1) as usize
    };
    let n = (2 * r + 1) as f32;
    let mut horiz = vec![0.0f32; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut s = 0.0;
            for dx in -r..=r {
                s += plane[idx(x + dx, y)];
            }
            horiz[idx(x, y)] = s / n;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut s = 0.0;
            for dy in -r..=r {
                s += horiz[idx(x, y + dy)];
            }
            out[idx(x, y)] = s / n;
        }
    }
    out
}

/// Rec.709 luma of a display-linear RGB triple.
fn luma709_px(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Refined dehaze transmission map `q` (design steps 1–5): normalized dark channel
/// → separable block-min over `radius` → guided-filter refinement (guide = luma).
/// Pure CPU reference the WGSL passes are golden-tested against. Deterministic.
pub fn transmission_map(
    img: &[[f32; 3]],
    w: usize,
    h: usize,
    a: [f32; 3],
    radius: u32,
) -> Vec<f32> {
    let n = w * h;
    let af = [
        a[0].max(DEHAZE_ATMOS_MIN),
        a[1].max(DEHAZE_ATMOS_MIN),
        a[2].max(DEHAZE_ATMOS_MIN),
    ];
    // 1. normalized dark channel; 4. guide (luma)
    let mut dc0 = vec![0.0f32; n];
    let mut guide = vec![0.0f32; n];
    for i in 0..n {
        let c = img[i];
        dc0[i] = (c[0] / af[0]).min(c[1] / af[1]).min(c[2] / af[2]);
        guide[i] = luma709_px(c);
    }
    // 2. block min (separable)
    let dc = min_filter_separable(&dc0, w, h, radius as i32);
    // 3. raw transmission
    let praw: Vec<f32> = dc
        .iter()
        .map(|&d| (1.0 - DEHAZE_OMEGA * d).clamp(0.0, 1.0))
        .collect();
    // 5. guided filter (guide = luma), window gr, eps
    let gr = guided_radius(radius) as i32;
    let gg: Vec<f32> = guide.iter().map(|&g| g * g).collect();
    let gp: Vec<f32> = guide.iter().zip(&praw).map(|(&g, &p)| g * p).collect();
    let mean_g = box_blur_separable(&guide, w, h, gr);
    let mean_p = box_blur_separable(&praw, w, h, gr);
    let corr_g = box_blur_separable(&gg, w, h, gr);
    let corr_gp = box_blur_separable(&gp, w, h, gr);
    let mut av = vec![0.0f32; n];
    let mut bv = vec![0.0f32; n];
    for i in 0..n {
        let var_g = corr_g[i] - mean_g[i] * mean_g[i];
        let cov_gp = corr_gp[i] - mean_g[i] * mean_p[i];
        av[i] = cov_gp / (var_g + DEHAZE_GUIDED_EPS);
        bv[i] = mean_p[i] - av[i] * mean_g[i];
    }
    let mean_a = box_blur_separable(&av, w, h, gr);
    let mean_b = box_blur_separable(&bv, w, h, gr);
    (0..n)
        .map(|i| (mean_a[i] * guide[i] + mean_b[i]).clamp(0.0, 1.0))
        .collect()
}

/// Halo (px) a tiled full-res dehaze pass must over-fetch: the op's patch radius
/// plus the guided filter window (clamped) when active, else 0 (mirrors `sharpen_halo`).
/// Consumed by the tile producer; a radius change therefore triggers `needs_full_rebuild`,
/// an amount-only change does not.
pub fn dehaze_halo(op: Option<Dehaze>) -> u32 {
    match op {
        Some(d) if d.amount != 0.0 => {
            let r = d.radius.min(MAX_DEHAZE_RADIUS);
            r + 2 * guided_radius(r)
        }
        _ => 0,
    }
}

/// GPU uniform for `dehaze.wgsl`. `#[repr(C)]`, 16-byte aligned; field order +
/// padding MIRROR the WGSL `struct P` exactly. `atmos` is `[r, g, b, pad]`.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DehazeUniform {
    pub amount: f32,
    pub radius: i32,
    pub omega: f32,
    pub t0: f32,
    pub atmos: [f32; 4],
}

/// Build the dehaze uniform from the op + the whole-image atmospheric light.
/// Absent/identity op → `amount 0`, `radius 0` (the shader takes its passthrough
/// branch). `atmos` is floored so the shader's `I/A` division is finite.
pub(crate) fn dehaze_uniform(op: Option<Dehaze>, atmos: [f32; 3]) -> DehazeUniform {
    let (amount, r) = op.map(|d| (d.amount, d.radius)).unwrap_or((0.0, 0));
    // A no-op amount contributes no radius (shader passthrough); otherwise clamp.
    let radius = if amount != 0.0 {
        r.min(MAX_DEHAZE_RADIUS) as i32
    } else {
        0
    };
    DehazeUniform {
        amount,
        radius,
        omega: DEHAZE_OMEGA,
        t0: DEHAZE_T0,
        atmos: [
            atmos[0].max(DEHAZE_ATMOS_MIN),
            atmos[1].max(DEHAZE_ATMOS_MIN),
            atmos[2].max(DEHAZE_ATMOS_MIN),
            0.0,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: u32, h: u32, rgb: [f32; 3]) -> LinearRgbaF32 {
        let mut px = Vec::with_capacity((w * h) as usize * 4);
        for _ in 0..(w * h) {
            px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
        }
        LinearRgbaF32::new(w, h, px).unwrap()
    }

    #[test]
    fn dark_channel_is_min_of_rgb() {
        assert_eq!(dark_channel([0.2, 0.5, 0.9]), 0.2);
        assert_eq!(dark_channel([0.7, 0.3, 0.6]), 0.3);
    }

    #[test]
    fn recover_is_identity_at_zero_amount() {
        let px = [0.4, 0.5, 0.6];
        let out = dehaze_recover(px, 0.5, [0.9, 0.9, 0.9], 0.0);
        for c in 0..3 {
            assert!((out[c] - px[c]).abs() < 1e-6, "amount 0 must be identity");
        }
    }

    #[test]
    fn positive_amount_pushes_away_from_atmosphere() {
        // A hazy mid-grey pixel under a bright atmosphere: removing haze must move
        // it AWAY from A (darker here, since px < A) — i.e. increased contrast.
        let px = [0.6, 0.6, 0.6];
        let a = [0.9, 0.9, 0.9];
        let dark = 0.6; // normalized dark channel (I/A) ~ 0.6/0.9
        let out = dehaze_recover(px, dark, a, 1.0);
        assert!(
            out[0] < px[0],
            "haze removal moves a below-A pixel down: {out:?}"
        );
    }

    #[test]
    fn negative_amount_pulls_toward_atmosphere() {
        // Adding haze pulls the pixel TOWARD A (lower contrast).
        let px = [0.3, 0.3, 0.3];
        let a = [0.9, 0.9, 0.9];
        let out = dehaze_recover(px, 0.6, a, -1.0);
        assert!(
            out[0] > px[0],
            "adding haze lifts a below-A pixel toward A: {out:?}"
        );
        assert!(out[0] <= a[0] + 1e-6);
    }

    #[test]
    fn recover_roundtrips_toward_identity_near_zero() {
        // Small +/- amounts straddle the input (monotone in amount at fixed dark/A).
        let px = [0.5, 0.4, 0.55];
        let a = [0.85, 0.85, 0.85];
        let up = dehaze_recover(px, 0.5, a, 0.2);
        let down = dehaze_recover(px, 0.5, a, -0.2);
        // +amount (remove) moves away from A; -amount (add) moves toward A.
        assert!(up[0] < px[0] && down[0] > px[0]);
    }

    #[test]
    fn estimate_atmosphere_picks_the_bright_hazy_region() {
        // A dark scene (low dark-channel) with a bright hazy sky patch: A should
        // track the bright patch, not the dark foreground.
        let mut img = flat(64, 64, [0.05, 0.05, 0.06]);
        // Top 8 rows = bright haze.
        for y in 0..8u32 {
            for x in 0..64u32 {
                let i = ((y * 64 + x) * 4) as usize;
                img.pixels[i] = 0.9;
                img.pixels[i + 1] = 0.92;
                img.pixels[i + 2] = 0.95;
            }
        }
        let a = estimate_atmospheric_light(&img);
        assert!(
            a[0] > 0.7 && a[1] > 0.7 && a[2] > 0.7,
            "A tracks the bright haze: {a:?}"
        );
    }

    #[test]
    fn estimate_atmosphere_is_floored_not_zero() {
        let a = estimate_atmospheric_light(&flat(8, 8, [0.0, 0.0, 0.0]));
        assert!(
            a.iter().all(|&c| c >= DEHAZE_ATMOS_MIN),
            "A is floored: {a:?}"
        );
    }

    #[test]
    fn dehaze_halo_is_op_radius_or_zero() {
        assert_eq!(dehaze_halo(None), 0);
        // amount 0 contributes no halo even with a radius set.
        assert_eq!(
            dehaze_halo(Some(Dehaze {
                amount: 0.0,
                radius: 10
            })),
            0
        );
        // With guided filter: halo = r + 2*gr(r).
        assert_eq!(
            dehaze_halo(Some(Dehaze {
                amount: 0.5,
                radius: 10
            })),
            10 + 2 * guided_radius(10)
        );
        assert_eq!(
            dehaze_halo(Some(Dehaze {
                amount: -0.5,
                radius: 6
            })),
            6 + 2 * guided_radius(6)
        );
        // Clamped to MAX_DEHAZE_RADIUS (no u32→i32 wrap).
        assert_eq!(
            dehaze_halo(Some(Dehaze {
                amount: 0.5,
                radius: u32::MAX
            })),
            MAX_DEHAZE_RADIUS + 2 * guided_radius(MAX_DEHAZE_RADIUS)
        );
    }

    #[test]
    fn dehaze_uniform_identity_and_layout() {
        let u = dehaze_uniform(None, DEHAZE_ATMOS_NEUTRAL);
        assert_eq!(u.amount, 0.0);
        assert_eq!(u.radius, 0);
        // 32 bytes, 16-aligned (mirrors the WGSL `struct P`).
        assert_eq!(std::mem::size_of::<DehazeUniform>(), 32);
        assert_eq!(std::mem::size_of::<DehazeUniform>() % 16, 0);
        // Present op carries its OWN radius (clamped) + floored atmosphere.
        let u2 = dehaze_uniform(
            Some(Dehaze {
                amount: 0.5,
                radius: 12,
            }),
            [0.0, 0.5, 1.0],
        );
        assert_eq!(u2.radius, 12);
        assert!(u2.atmos[0] >= DEHAZE_ATMOS_MIN);
        assert_eq!(u2.atmos[1], 0.5);
        let u3 = dehaze_uniform(
            Some(Dehaze {
                amount: 0.5,
                radius: u32::MAX,
            }),
            DEHAZE_ATMOS_NEUTRAL,
        );
        assert_eq!(u3.radius, MAX_DEHAZE_RADIUS as i32);
    }

    #[test]
    fn min_filter_separable_matches_naive_patch_min() {
        // 6x5 plane, radius 2: separable (H then V) min == naive (2r+1)^2 patch min.
        let (w, h) = (6usize, 5usize);
        let plane: Vec<f32> = (0..w * h).map(|i| ((i * 37) % 11) as f32 / 11.0).collect();
        let sep = min_filter_separable(&plane, w, h, 2);
        // naive reference
        let mut naive = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut m = f32::INFINITY;
                for dy in -2i32..=2 {
                    for dx in -2i32..=2 {
                        let qx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                        let qy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                        m = m.min(plane[qy * w + qx]);
                    }
                }
                naive[y * w + x] = m;
            }
        }
        for (a, b) in sep.iter().zip(naive.iter()) {
            assert!(
                (a - b).abs() < 1e-6,
                "separable min must equal naive patch min"
            );
        }
    }

    #[test]
    fn transmission_identity_on_flat_image_has_no_structure() {
        // A flat grey image → transmission is spatially constant (no halos, no NaN).
        let (w, h) = (16usize, 16usize);
        let img = vec![[0.5f32, 0.5, 0.5]; w * h];
        let q = transmission_map(&img, w, h, [0.9, 0.9, 0.9], 4);
        let first = q[0];
        for &v in &q {
            assert!(v.is_finite());
            assert!((v - first).abs() < 1e-4, "flat image → flat transmission");
        }
    }

    #[test]
    fn guided_refinement_removes_most_of_the_block_min_halo() {
        // Vertical luma edge (left dark 0.05, right bright 0.9). The raw block-min
        // transmission dilates the dark region `radius` px into the bright side (a
        // bright halo). The guided-filter refinement, keyed on the luma guide, must
        // pull that halo band back toward the clean bright-field transmission.
        let (w, h) = (64usize, 8usize);
        let mut img = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.05 } else { 0.9 };
                img[y * w + x] = [v, v, v];
            }
        }
        let a = [0.9, 0.9, 0.9];
        let radius = 6u32;
        let edge = w / 2;
        // Raw (unrefined) block-min transmission, for comparison.
        let n = w * h;
        let mut dc0 = vec![0.0f32; n];
        for i in 0..n {
            let c = img[i];
            dc0[i] = (c[0] / a[0]).min(c[1] / a[1]).min(c[2] / a[2]);
        }
        let dc = min_filter_separable(&dc0, w, h, radius as i32);
        let praw: Vec<f32> = dc
            .iter()
            .map(|&d| (1.0 - DEHAZE_OMEGA * d).clamp(0.0, 1.0))
            .collect();
        let q = transmission_map(&img, w, h, a, radius);
        let row = (h / 2) * w;
        let clean = q[row + w - 1]; // far bright-field refined transmission
        let halo_x = row + edge + radius as usize / 2; // squarely in the dilated band
        let raw_halo = praw[halo_x];
        let refined_halo = q[halo_x];
        assert!(
            raw_halo - clean > 0.3,
            "raw block-min must have a real halo to remove (raw={raw_halo}, clean={clean})"
        );
        let removed = (raw_halo - refined_halo) / (raw_halo - clean);
        assert!(
            removed >= 0.6,
            "guided filter must remove >=60% of the halo at its location \
             (removed {:.0}%, raw={raw_halo}, refined={refined_halo}, clean={clean})",
            removed * 100.0
        );
    }

    #[test]
    fn dehaze_halo_includes_guided_window() {
        // Halo now covers the block-min radius PLUS the two guided-filter box windows.
        assert_eq!(
            dehaze_halo(Some(Dehaze {
                amount: 0.5,
                radius: 8
            })),
            8 + 2 * guided_radius(8)
        );
        assert_eq!(
            dehaze_halo(Some(Dehaze {
                amount: 0.0,
                radius: 8
            })),
            0
        );
    }
}
