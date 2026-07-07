//! Full-resolution "RCD-family" Bayer demosaic (ratio/gradient-corrected
//! directional): Hamilton-Adams directional green interpolation + constant-hue
//! (colour-difference) red/blue interpolation. RGGB only; other CFA patterns and
//! non-Bayer sensors fall back to the half-res `QuadBin` path (spec §5.2). Output
//! is display-linear, white-balanced, and UNCLAMPED (carries highlights >1 and
//! wide-gamut negatives — P2 §5.3). This CPU impl is the golden reference the
//! Plan-5 WGSL RCD pass is validated against. Parallelised per output row with
//! rayon; bit-identical to serial. (`wide` SIMD is a deferred perf follow-up — §8.)

use crate::demosaic::{DemosaicParams, DemosaicToRgb16f, QuadBin};
use crate::raw::RawDecoded;
use ferrolite_image::LinearRgbaF32;
use rayon::prelude::*;

/// Below this output pixel count, run serially (rayon overhead not worth it).
/// Mirrors `QuadBin`'s threshold.
const PARALLEL_MIN_PIXELS: u64 = 65_536;

/// The RGGB CFA pattern (the only pattern RCD handles; others fall back).
const RGGB: [u8; 4] = [0, 1, 1, 2];

/// Full-res "RCD-family" demosaic; delegates to `QuadBin` for non-RGGB sensors.
pub struct Rcd;

impl DemosaicToRgb16f for Rcd {
    fn to_linear_rgba_f32(&self, raw: &RawDecoded) -> LinearRgbaF32 {
        if raw.cfa_pattern != RGGB {
            // Non-RGGB / X-Trans / non-Bayer: the existing half-res path (spec §5.2).
            return QuadBin.to_linear_rgba_f32(raw);
        }
        demosaic_rggb(raw)
    }
}

fn demosaic_rggb(raw: &RawDecoded) -> LinearRgbaF32 {
    let w = raw.width as usize;
    let h = raw.height as usize;
    let p = DemosaicParams::from_raw(raw);
    let span = (p.white_level - p.black_levels[0]).max(1.0);

    // Black-subtracted, normalized single-channel CFA (NOT white-balanced yet —
    // WB is applied to the interpolated output so interpolation runs on
    // sensor-linear values). Floor at 0 is the black point (not a gamut clip).
    let c: Vec<f32> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let pos = (y % 2) * 2 + (x % 2);
            ((raw.pixels[i] as f32 - p.black_levels[pos]) / span).max(0.0)
        })
        .collect();

    // Pass 1: green at every pixel (measured at G sites; directional at R/B).
    let green = interpolate_green(&c, w, h);

    // Pass 2: full RGB per pixel via colour-difference chroma, then apply WB.
    // Each output pixel is a pure function of `c` and `green`, so the per-row
    // rayon path is bit-identical to serial.
    let mut out = vec![0.0f32; LinearRgbaF32::expected_len(raw.width, raw.height)];
    let row_stride = w * 4;
    let fill_row = |y: usize, row: &mut [f32]| {
        for x in 0..w {
            let (r, g, b) = reconstruct_rgb(&c, &green, w, h, x, y);
            let base = x * 4;
            row[base] = r * p.wb_coeffs[0];
            row[base + 1] = g * p.wb_coeffs[1];
            row[base + 2] = b * p.wb_coeffs[2];
            row[base + 3] = 1.0;
        }
    };
    let total = (w as u64) * (h as u64);
    if total >= PARALLEL_MIN_PIXELS {
        out.par_chunks_mut(row_stride)
            .enumerate()
            .for_each(|(y, row)| fill_row(y, row));
    } else {
        for (y, row) in out.chunks_mut(row_stride).enumerate() {
            fill_row(y, row);
        }
    }
    LinearRgbaF32::new(raw.width, raw.height, out).expect("rcd length matches dims")
}

/// Read the normalized CFA at `(x, y)` with edge-replication clamping.
#[inline]
fn sample(c: &[f32], w: usize, h: usize, x: i32, y: i32) -> f32 {
    let xc = x.clamp(0, w as i32 - 1) as usize;
    let yc = y.clamp(0, h as i32 - 1) as usize;
    c[yc * w + xc]
}

/// Directional green at every pixel: measured at G sites; Hamilton-Adams
/// horizontal-vs-vertical estimate (bilinear green + same-colour Laplacian
/// correction) at R and B sites, choosing the lower-gradient direction.
fn interpolate_green(c: &[f32], w: usize, h: usize) -> Vec<f32> {
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            let pos = ((y as usize) % 2) * 2 + ((x as usize) % 2);
            if pos == 1 || pos == 2 {
                return c[i]; // G site: measured green
            }
            let s = |dx: i32, dy: i32| sample(c, w, h, x + dx, y + dy);
            let center = s(0, 0);
            let gh = (s(-1, 0) - s(1, 0)).abs() + (2.0 * center - s(-2, 0) - s(2, 0)).abs();
            let gv = (s(0, -1) - s(0, 1)).abs() + (2.0 * center - s(0, -2) - s(0, 2)).abs();
            let gh_est = 0.5 * (s(-1, 0) + s(1, 0)) + 0.25 * (2.0 * center - s(-2, 0) - s(2, 0));
            let gv_est = 0.5 * (s(0, -1) + s(0, 1)) + 0.25 * (2.0 * center - s(0, -2) - s(0, 2));
            if gh < gv {
                gh_est
            } else if gv < gh {
                gv_est
            } else {
                0.5 * (gh_est + gv_est)
            }
        })
        .collect()
}

/// Full sensor-linear (pre-WB) `(R, G, B)` at `(x, y)` from the normalized CFA
/// and the interpolated green, via constant-hue (colour-difference) chroma.
fn reconstruct_rgb(
    c: &[f32],
    green: &[f32],
    w: usize,
    h: usize,
    x: usize,
    y: usize,
) -> (f32, f32, f32) {
    let (xi, yi) = (x as i32, y as i32);
    let pos = (y % 2) * 2 + (x % 2);
    let cs = |dx: i32, dy: i32| sample(c, w, h, xi + dx, yi + dy);
    let gs = |dx: i32, dy: i32| sample(green, w, h, xi + dx, yi + dy);
    let g_here = green[y * w + x];
    match pos {
        0 => {
            // R site: R measured; B from the 4 diagonal B neighbours (colour diff).
            let r = cs(0, 0);
            let b = g_here
                + 0.25
                    * ((cs(-1, -1) - gs(-1, -1))
                        + (cs(1, -1) - gs(1, -1))
                        + (cs(-1, 1) - gs(-1, 1))
                        + (cs(1, 1) - gs(1, 1)));
            (r, g_here, b)
        }
        3 => {
            // B site: B measured; R from the 4 diagonal R neighbours (colour diff).
            let b = cs(0, 0);
            let r = g_here
                + 0.25
                    * ((cs(-1, -1) - gs(-1, -1))
                        + (cs(1, -1) - gs(1, -1))
                        + (cs(-1, 1) - gs(-1, 1))
                        + (cs(1, 1) - gs(1, 1)));
            (r, g_here, b)
        }
        1 => {
            // G site (even row, odd col): R horizontal neighbours, B vertical.
            let g = cs(0, 0);
            let r = g + 0.5 * ((cs(-1, 0) - gs(-1, 0)) + (cs(1, 0) - gs(1, 0)));
            let b = g + 0.5 * ((cs(0, -1) - gs(0, -1)) + (cs(0, 1) - gs(0, 1)));
            (r, g, b)
        }
        _ => {
            // pos == 2: G site (odd row, even col): B horizontal, R vertical.
            let g = cs(0, 0);
            let b = g + 0.5 * ((cs(-1, 0) - gs(-1, 0)) + (cs(1, 0) - gs(1, 0)));
            let r = g + 0.5 * ((cs(0, -1) - gs(0, -1)) + (cs(0, 1) - gs(0, 1)));
            (r, g, b)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an RGGB `RawDecoded`: black 0, white 65535, given WB, `pixels` row-major.
    fn raw_rggb(w: u32, h: u32, pixels: Vec<u16>, wb: [f32; 4]) -> RawDecoded {
        assert_eq!(pixels.len(), (w * h) as usize);
        RawDecoded {
            width: w,
            height: h,
            cpp: 1,
            pixels,
            cfa_pattern: [0, 1, 1, 2],
            black_levels: [0.0; 4],
            white_level: 65535.0,
            wb_coeffs: wb,
            color_profile: crate::color::ColorProfile::srgb_fallback(),
            orientation: ferrolite_image::Orientation::Normal,
        }
    }

    #[test]
    fn rcd_is_full_resolution() {
        let raw = raw_rggb(8, 6, vec![1000u16; 48], [1.0; 4]);
        let out = Rcd.to_linear_rgba_f32(&raw);
        assert_eq!(
            (out.width, out.height),
            (8, 6),
            "RCD is full-res (not half like QuadBin)"
        );
        assert_eq!(out.pixels.len(), LinearRgbaF32::expected_len(8, 6));
    }

    #[test]
    fn rcd_flat_field_reconstructs_uniform_after_wb() {
        // A uniform sensor → every output pixel is the same WB'd value on each channel.
        let raw = raw_rggb(8, 8, vec![30000u16; 64], [1.0, 1.0, 1.0, 1.0]);
        let out = Rcd.to_linear_rgba_f32(&raw);
        let v = 30000.0 / 65535.0;
        for i in 0..64 {
            for c in 0..3 {
                assert!(
                    (out.pixels[i * 4 + c] - v).abs() < 1e-4,
                    "px {i} ch {c} = {}",
                    out.pixels[i * 4 + c]
                );
            }
        }
    }

    #[test]
    fn rcd_reconstructs_neutral_horizontal_ramp() {
        // Neutral scene ramp: every pixel samples the same underlying value s(x)=x*1000,
        // so a correct demosaic yields R≈G≈B≈s(x)/white at interior pixels (exact for a
        // linear ramp under Hamilton-Adams + constant-hue; borders excluded).
        let (w, h) = (16u32, 16u32);
        let pixels: Vec<u16> = (0..w * h).map(|i| ((i % w) as u16) * 1000).collect();
        let out = Rcd.to_linear_rgba_f32(&raw_rggb(w, h, pixels, [1.0; 4]));
        for y in 2..(h - 2) {
            for x in 2..(w - 2) {
                let want = (x as f32 * 1000.0) / 65535.0;
                let i = (y * w + x) as usize;
                for c in 0..3 {
                    assert!(
                        (out.pixels[i * 4 + c] - want).abs() < 1e-4,
                        "interior px ({x},{y}) ch {c}: want {want} got {}",
                        out.pixels[i * 4 + c]
                    );
                }
            }
        }
    }

    #[test]
    fn rcd_preserves_values_above_one() {
        // Bright field + WB > 1 pushes the red channel past 1.0; RCD must carry it.
        let raw = raw_rggb(6, 6, vec![65535u16; 36], [2.0, 1.0, 1.0, 1.0]);
        let out = Rcd.to_linear_rgba_f32(&raw);
        // Pixel 0 is an R site: R = 1.0 * wb_R(2.0) = 2.0, carried unclamped.
        assert!(
            (out.pixels[0] - 2.0).abs() < 1e-4,
            "R must carry >1 (got {})",
            out.pixels[0]
        );
    }

    #[test]
    fn rcd_non_rggb_falls_back_to_quadbin() {
        // A BGGR sensor is not handled by RCD → delegates to QuadBin (half-res).
        let mut raw = raw_rggb(
            8,
            8,
            (0..64).map(|i| (i * 100) as u16).collect(),
            [1.3, 1.0, 1.1, 1.0],
        );
        raw.cfa_pattern = [2, 1, 1, 0]; // BGGR
        let rcd_out = Rcd.to_linear_rgba_f32(&raw);
        let qb_out = QuadBin.to_linear_rgba_f32(&raw);
        assert_eq!(
            (rcd_out.width, rcd_out.height),
            (qb_out.width, qb_out.height),
            "non-RGGB falls back to half-res QuadBin (4x4, not 8x8)"
        );
        assert_eq!(
            rcd_out.pixels, qb_out.pixels,
            "fallback returns exactly QuadBin output"
        );
    }

    #[test]
    fn rcd_parallel_matches_serial_reference() {
        // 256x256 output ≥ PARALLEL_MIN_PIXELS exercises the rayon path. Recompute
        // every pixel serially (same core helpers) and require bit-identical output,
        // proving per-row parallelism doesn't reorder/corrupt.
        let (w, h) = (256u32, 256u32);
        let pixels: Vec<u16> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                ((x.wrapping_mul(7) + y.wrapping_mul(13) + x * y) % 4001) as u16
            })
            .collect();
        let wb = [1.8, 1.0, 1.4, 1.0];
        let raw = raw_rggb(w, h, pixels, wb);
        let out = Rcd.to_linear_rgba_f32(&raw); // parallel (above threshold)

        let (wu, hu) = (w as usize, h as usize);
        let p = DemosaicParams::from_raw(&raw);
        let span = (p.white_level - p.black_levels[0]).max(1.0);
        let c: Vec<f32> = (0..wu * hu)
            .map(|i| {
                let (x, y) = (i % wu, i / wu);
                let pos = (y % 2) * 2 + (x % 2);
                ((raw.pixels[i] as f32 - p.black_levels[pos]) / span).max(0.0)
            })
            .collect();
        let green = interpolate_green(&c, wu, hu);
        let mut expected = vec![0.0f32; LinearRgbaF32::expected_len(w, h)];
        for y in 0..hu {
            for x in 0..wu {
                let (r, g, b) = reconstruct_rgb(&c, &green, wu, hu, x, y);
                let base = (y * wu + x) * 4;
                expected[base] = r * p.wb_coeffs[0];
                expected[base + 1] = g * p.wb_coeffs[1];
                expected[base + 2] = b * p.wb_coeffs[2];
                expected[base + 3] = 1.0;
            }
        }
        assert_eq!(
            out.pixels, expected,
            "parallel output must be bit-identical to serial"
        );
    }
}
