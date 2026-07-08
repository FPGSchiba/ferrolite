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
const DEHAZE_OMEGA: f32 = 0.95;
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
    // Brightest 0.1% by dark channel (at least one).
    let keep = (samples.len() / 1000).max(1);
    samples.sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
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

/// Halo (px) a tiled full-res dehaze pass must over-fetch: the op's patch radius
/// (clamped) when active, else 0 (mirrors `sharpen_halo`). Consumed by the tile
/// producer; a radius change therefore triggers `needs_full_rebuild`, an
/// amount-only change does not.
pub fn dehaze_halo(op: Option<Dehaze>) -> u32 {
    match op {
        Some(d) if d.amount != 0.0 => d.radius.min(MAX_DEHAZE_RADIUS),
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
#[allow(dead_code)]
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
        assert_eq!(
            dehaze_halo(Some(Dehaze {
                amount: 0.5,
                radius: 10
            })),
            10
        );
        assert_eq!(
            dehaze_halo(Some(Dehaze {
                amount: -0.5,
                radius: 6
            })),
            6
        );
        // Clamped to MAX_DEHAZE_RADIUS (no u32→i32 wrap).
        assert_eq!(
            dehaze_halo(Some(Dehaze {
                amount: 0.5,
                radius: u32::MAX
            })),
            MAX_DEHAZE_RADIUS
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
}
