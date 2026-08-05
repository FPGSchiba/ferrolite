//! Pure CPU reference for the à trous wavelet noise reduction (P4 design §3.2–§3.4).
//! No GPU types: this module is the correctness oracle the WGSL passes in
//! `nr_node.rs` are goldened against, exactly as `dehaze::transmission_map` is
//! the oracle for the dehaze passes.

/// Wavelet decomposition levels (design constant — halo derives from it).
pub const NR_LEVELS: usize = 5;

/// The factor by which unit-variance white noise's standard deviation survives
/// into each à trous level of a B3-spline decomposition. Using these means ONE
/// strength slider yields a physically consistent threshold at every scale.
/// These are the standard B3-spline ([1,4,6,4,1]/16) noise-propagation table
/// (design §3.4), not fitted to this codebase. `white_noise_variance_drops`
/// checks only an AGGREGATE variance reduction on synthetic white noise — it
/// would pass for almost any roughly-decaying constants, so it does not (and
/// is not meant to) verify these specific per-level values; the per-level
/// shape is instead the literature table itself.
pub const NR_NOISE_SCALE: [f32; NR_LEVELS] = [0.890, 0.201, 0.086, 0.041, 0.020];

/// Slider→scene-linear-threshold scale (final-review FIX 4, author decision).
/// `t_l = strength · s_l` uses `NR_NOISE_SCALE`, the noise-propagation table
/// for UNIT-VARIANCE noise, but the Luminance/Color sliders feed `strength`
/// raw as a `0..1` value in scene-linear working-space units. Real RAW noise
/// sits at σ ≈ 0.005–0.02 linear, so the threshold that actually matters is
/// ≈`3σ·s_l`, i.e. an effective `strength` of ≈0.02–0.06 — without this
/// scale, only the bottom few percent of the slider's travel did anything
/// useful and the rest destroyed detail. This is the SINGLE tuning knob for
/// NR strength (same role as `KEYSTONE_STRENGTH`/`SHARPEN_MASK_GRADIENT_NORM`
/// — change ONLY this constant to retune, not the formula). The author may
/// re-tune this value after hands-on testing at ISO 3200–6400; it is not
/// claimed to be final.
pub const NR_STRENGTH_SCALE: f32 = 0.05;

/// The B3-spline kernel [1,4,6,4,1]/16.
const B3: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// Halo (pixels) a tiled NR pass must over-fetch: the total support of `NR_LEVELS`
/// à trous levels. Level `l` uses a 5-tap kernel at spacing `2^l`, so radius
/// `2·2^l`; summing gives `2·(2^L − 1)`.
pub fn nr_halo_px() -> u32 {
    2 * ((1u32 << NR_LEVELS) - 1)
}

/// `t_l = NR_STRENGTH_SCALE · strength · s_l · f(detail, l)`,
/// `f = 1 − detail·max(0, 1 − l/2)`. `detail = 1` zeroes level 0, halves
/// level 1, leaves `l >= 2` untouched. `NR_STRENGTH_SCALE` maps the slider's
/// raw `0..1` range onto the useful scene-linear threshold band (see its doc).
pub fn threshold_at(strength: f32, detail: f32, level: usize) -> f32 {
    let s_l = NR_NOISE_SCALE[level.min(NR_LEVELS - 1)];
    let f = 1.0 - detail * (1.0 - level as f32 / 2.0).max(0.0);
    NR_STRENGTH_SCALE * strength * s_l * f
}

/// Soft shrinkage. Hard thresholding is what produces the "plastic" look.
pub fn shrink(d: f32, t: f32) -> f32 {
    let m = d.abs() - t;
    if m <= 0.0 {
        0.0
    } else if d < 0.0 {
        -m
    } else {
        m
    }
}

fn clamp_idx(v: isize, n: usize) -> usize {
    v.clamp(0, n as isize - 1) as usize
}

/// Horizontal B3-spline convolution at hole spacing `spacing`, clamping x only.
pub fn b3_spline_h(src: &[f32], w: usize, h: usize, spacing: usize) -> Vec<f32> {
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, coeff) in B3.iter().enumerate() {
                let dx = (k as isize - 2) * spacing as isize;
                acc += coeff * src[y * w + clamp_idx(x as isize + dx, w)];
            }
            out[y * w + x] = acc;
        }
    }
    out
}

/// Vertical B3-spline convolution at hole spacing `spacing`, clamping y only.
pub fn b3_spline_v(src: &[f32], w: usize, h: usize, spacing: usize) -> Vec<f32> {
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, coeff) in B3.iter().enumerate() {
                let dy = (k as isize - 2) * spacing as isize;
                acc += coeff * src[clamp_idx(y as isize + dy, h) * w + x];
            }
            out[y * w + x] = acc;
        }
    }
    out
}

/// Direct (non-separable) 2D B3-spline convolution — the oracle proving the
/// H-then-V composition above is equivalent.
pub fn b3_spline_2d(src: &[f32], w: usize, h: usize, spacing: usize) -> Vec<f32> {
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (ky, cy) in B3.iter().enumerate() {
                let dy = (ky as isize - 2) * spacing as isize;
                let yy = clamp_idx(y as isize + dy, h);
                for (kx, cx) in B3.iter().enumerate() {
                    let dx = (kx as isize - 2) * spacing as isize;
                    acc += cy * cx * src[yy * w + clamp_idx(x as isize + dx, w)];
                }
            }
            out[y * w + x] = acc;
        }
    }
    out
}

/// The full streaming à trous shrink of one scalar plane (design §3.3):
/// shrinkage is fused into the decomposition loop, so no level is retained.
pub fn atrous_shrink_reference(
    plane: &[f32],
    w: usize,
    h: usize,
    strength: f32,
    detail: f32,
) -> Vec<f32> {
    if strength <= 0.0 {
        return plane.to_vec();
    }
    let mut approx = plane.to_vec();
    let mut acc = vec![0.0f32; w * h];
    for l in 0..NR_LEVELS {
        let spacing = 1usize << l;
        let next = b3_spline_v(&b3_spline_h(&approx, w, h, spacing), w, h, spacing);
        let t = threshold_at(strength, detail, l);
        for i in 0..w * h {
            acc[i] += shrink(approx[i] - next[i], t);
        }
        approx = next;
    }
    for i in 0..w * h {
        acc[i] += approx[i];
    }
    acc
}

/// Rec.709 luma / centred chroma. Chroma is centred on 0 (not 0.5) so a zero
/// coefficient means "no chroma", which keeps shrinkage sign-symmetric.
pub fn rgb_to_ycbcr(rgb: [f32; 3]) -> [f32; 3] {
    let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    [y, rgb[2] - y, rgb[0] - y]
}

/// Inverse of [`rgb_to_ycbcr`].
pub fn ycbcr_to_rgb(ycc: [f32; 3]) -> [f32; 3] {
    let (y, cb, cr) = (ycc[0], ycc[1], ycc[2]);
    let r = cr + y;
    let b = cb + y;
    let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    [r, g, b]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The separable H-then-V B3-spline convolution must equal the direct 2D
    /// form. Mirrors `sharpen_node.rs`'s `separable_box_equals_2d_box`, which
    /// proved the same property for the box mean before the GPU passes existed.
    #[test]
    fn separable_b3spline_equals_direct() {
        let (w, h) = (24usize, 18usize);
        let src: Vec<f32> = (0..w * h)
            .map(|i| {
                let x = (i % w) as f32;
                let y = (i / w) as f32;
                (x * 0.13).sin() * 0.5 + (y * 0.31).cos() * 0.3 + 0.5
            })
            .collect();
        for spacing in [1usize, 2, 4, 8, 16] {
            let sep = b3_spline_v(&b3_spline_h(&src, w, h, spacing), w, h, spacing);
            let direct = b3_spline_2d(&src, w, h, spacing);
            for (i, (a, b)) in sep.iter().zip(direct.iter()).enumerate() {
                assert!(
                    (a - b).abs() < 1e-6,
                    "spacing {spacing} idx {i}: separable {a} vs direct {b}"
                );
            }
        }
    }

    /// A flat image has no detail at any scale, so shrinkage cannot change it.
    #[test]
    fn flat_image_is_unchanged_by_any_strength() {
        let (w, h) = (16usize, 16usize);
        let src = vec![0.42f32; w * h];
        for strength in [0.0f32, 0.5, 1.0] {
            let out = atrous_shrink_reference(&src, w, h, strength, 0.0);
            for v in &out {
                assert!((v - 0.42).abs() < 1e-6, "flat image changed at {strength}");
            }
        }
    }

    /// Zero strength is an exact identity — the guarantee the GPU node's
    /// early-return (Task 2) mirrors.
    #[test]
    fn zero_strength_is_identity() {
        let (w, h) = (20usize, 12usize);
        let src: Vec<f32> = (0..w * h)
            .map(|i| ((i * 37) % 101) as f32 / 101.0)
            .collect();
        let out = atrous_shrink_reference(&src, w, h, 0.0, 0.0);
        for (a, b) in out.iter().zip(src.iter()) {
            assert!((a - b).abs() < 1e-6, "zero strength was not identity");
        }
    }

    /// Denoising must actually reduce noise: white noise on a flat field has
    /// lower variance after shrinkage.
    #[test]
    fn white_noise_variance_drops() {
        let (w, h) = (64usize, 64usize);
        // Deterministic pseudo-noise (no rand dep): a simple LCG.
        let mut state = 12345u32;
        let src: Vec<f32> = (0..w * h)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                0.5 + ((state >> 16) as f32 / 65535.0 - 0.5) * 0.1
            })
            .collect();
        let var = |v: &[f32]| {
            let m = v.iter().sum::<f32>() / v.len() as f32;
            v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32
        };
        let out = atrous_shrink_reference(&src, w, h, 1.0, 0.0);
        assert!(
            var(&out) < var(&src) * 0.6,
            "variance {} not meaningfully below {}",
            var(&out),
            var(&src)
        );
    }

    /// `detail` protects fine scales: it zeroes level 0's threshold, halves
    /// level 1's, and never touches level >= 2.
    #[test]
    fn detail_attenuates_only_the_two_finest_levels() {
        let s = 1.0;
        assert!(
            threshold_at(s, 1.0, 0).abs() < 1e-9,
            "detail=1 zeroes level 0"
        );
        let half = threshold_at(s, 0.0, 1) * 0.5;
        assert!(
            (threshold_at(s, 1.0, 1) - half).abs() < 1e-6,
            "detail=1 halves level 1"
        );
        for l in 2..NR_LEVELS {
            assert_eq!(
                threshold_at(s, 1.0, l),
                threshold_at(s, 0.0, l),
                "level {l} must be untouched by detail"
            );
        }
    }

    /// Soft shrinkage, never hard: a coefficient just above the threshold
    /// survives as a SMALL value, it is not passed through at full magnitude.
    #[test]
    fn shrink_is_soft_not_hard() {
        assert_eq!(shrink(0.05, 0.10), 0.0, "below threshold -> zero");
        assert!((shrink(0.12, 0.10) - 0.02).abs() < 1e-6, "soft: |d| - t");
        assert!((shrink(-0.12, 0.10) + 0.02).abs() < 1e-6, "sign preserved");
    }

    #[test]
    fn ycbcr_round_trips() {
        for rgb in [
            [0.1, 0.5, 0.9],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.7, 0.2, 0.4],
        ] {
            let back = ycbcr_to_rgb(rgb_to_ycbcr(rgb));
            for i in 0..3 {
                assert!(
                    (back[i] - rgb[i]).abs() < 1e-5,
                    "round trip failed for {rgb:?}"
                );
            }
        }
    }

    #[test]
    fn noise_reduction_is_identity_only_when_all_zero() {
        use crate::local::NoiseReduction;
        assert!(NoiseReduction::default().is_identity());
        for nr in [
            NoiseReduction {
                luminance: 0.1,
                ..Default::default()
            },
            NoiseReduction {
                detail: 0.1,
                ..Default::default()
            },
            NoiseReduction {
                color: 0.1,
                ..Default::default()
            },
            NoiseReduction {
                color_detail: 0.1,
                ..Default::default()
            },
        ] {
            assert!(!nr.is_identity(), "{nr:?} must not be identity");
        }
    }
}
