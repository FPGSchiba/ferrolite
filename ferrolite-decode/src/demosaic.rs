//! CFA → display-linear RGBA conversion. Photo-domain (needs WB / black level),
//! so it lives here, not in the engine tier. `QuadBin` is the fast half-res
//! default; a full-res `Bilinear` impl is a future drop-in behind the trait.

use crate::raw::RawDecoded;
use ferrolite_image::LinearRgbaF32;
use rayon::prelude::*;

/// Below this output pixel count, run the row loop serially instead of
/// spawning rayon tasks. Avoids rayon overhead and job-pool contention for
/// small frames (thumbnails, tiny previews) where per-row parallelism would
/// cost more than it saves. ~256x256 output (i.e. a ~512x512 sensor crop for
/// this 2x2 quad-bin) is comfortably above "thumbnail" territory while still
/// being trivial to compute serially.
const PARALLEL_MIN_PIXELS: u64 = 65_536;

#[derive(Debug, Clone)]
pub struct DemosaicParams {
    pub black_levels: [f32; 4],
    pub white_level: f32,
    pub wb_coeffs: [f32; 4],
    pub cfa_pattern: [u8; 4],
}

impl DemosaicParams {
    pub fn from_raw(raw: &RawDecoded) -> Self {
        Self {
            black_levels: raw.black_levels,
            white_level: raw.white_level,
            wb_coeffs: raw.wb_coeffs,
            cfa_pattern: raw.cfa_pattern,
        }
    }
}

/// Convert raw CFA samples to a display-linear RGBA f32 image.
pub trait DemosaicToRgb16f {
    fn to_linear_rgba_f32(&self, raw: &RawDecoded) -> LinearRgbaF32;
}

/// Half-resolution 2×2 quad binning: each RGGB quad → one RGB pixel. Zero
/// demosaic artifacts; output is display-linear (gamma applied at the shader).
pub struct QuadBin;

impl DemosaicToRgb16f for QuadBin {
    fn to_linear_rgba_f32(&self, raw: &RawDecoded) -> LinearRgbaF32 {
        let out_w = (raw.width / 2).max(1);
        let out_h = (raw.height / 2).max(1);
        let p = DemosaicParams::from_raw(raw);
        // Locate R, the two greens, and B within the 2×2 pattern.
        let idx_of = |target: u8| p.cfa_pattern.iter().position(|&c| c == target);
        let r_pos = idx_of(0).unwrap_or(0);
        let b_pos = idx_of(2).unwrap_or(3);
        let greens: Vec<usize> = (0..4).filter(|&i| p.cfa_pattern[i] == 1).collect();
        let (g0, g1) = (
            greens.first().copied().unwrap_or(1),
            greens.get(1).copied().unwrap_or(2),
        );

        let span = (p.white_level - p.black_levels[0]).max(1.0);
        let sample = |x: u32, y: u32, quad_idx: usize| -> f32 {
            let (qx, qy) = (quad_idx % 2, quad_idx / 2);
            let px = (x * 2 + qx as u32).min(raw.width - 1);
            let py = (y * 2 + qy as u32).min(raw.height - 1);
            let raw_v = raw.pixels[(py * raw.width + px) as usize] as f32;
            let bl = p.black_levels[quad_idx];
            ((raw_v - bl) / span).max(0.0)
        };

        let wb = p.wb_coeffs;

        // Fills one output row (identical per-pixel math to the original
        // serial loop, same left-to-right write order) — shared by both the
        // serial and parallel paths below so they cannot drift apart.
        let compute_row = |y: u32, row: &mut [f32]| {
            for x in 0..out_w {
                let r = (sample(x, y, r_pos) * wb[0]).clamp(0.0, 1.0);
                let g = (((sample(x, y, g0) + sample(x, y, g1)) * 0.5) * wb[1]).clamp(0.0, 1.0);
                let b = (sample(x, y, b_pos) * wb[2]).clamp(0.0, 1.0);
                let base = x as usize * 4;
                row[base] = r;
                row[base + 1] = g;
                row[base + 2] = b;
                row[base + 3] = 1.0;
            }
        };

        let mut pixels = vec![0.0f32; LinearRgbaF32::expected_len(out_w, out_h)];
        let row_stride = out_w as usize * 4;
        let total_pixels = out_w as u64 * out_h as u64;
        if total_pixels >= PARALLEL_MIN_PIXELS {
            pixels
                .par_chunks_mut(row_stride)
                .enumerate()
                .for_each(|(y, row)| compute_row(y as u32, row));
        } else {
            for (y, row) in pixels.chunks_mut(row_stride).enumerate() {
                compute_row(y as u32, row);
            }
        }
        LinearRgbaF32::new(out_w, out_h, pixels).expect("quadbin length matches dims")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_image::LinearRgbaF32;

    /// Build a 2x2 RGGB RawDecoded with known samples and verify the single
    /// binned output pixel: R, avg(G1,G2), B, after black-level + WB + normalize.
    fn raw_2x2(r: u16, g1: u16, g2: u16, b: u16) -> crate::raw::RawDecoded {
        crate::raw::RawDecoded {
            width: 2,
            height: 2,
            cpp: 1,
            pixels: vec![r, g1, g2, b], // row0: R,G1 ; row1: G2,B
            cfa_pattern: [0, 1, 1, 2],  // RGGB
            black_levels: [0.0; 4],
            white_level: 100.0,
            wb_coeffs: [1.0, 1.0, 1.0, 1.0],
            color_profile: crate::color::ColorProfile::srgb_fallback(),
            orientation: ferrolite_image::Orientation::Normal,
        }
    }

    #[test]
    fn quadbin_halves_dimensions() {
        let raw = raw_2x2(100, 50, 50, 0);
        let out: LinearRgbaF32 = QuadBin.to_linear_rgba_f32(&raw);
        assert_eq!((out.width, out.height), (1, 1));
        assert_eq!(out.pixels.len(), 4);
    }

    #[test]
    fn quadbin_bins_channels_and_normalizes() {
        // white_level 100 -> R=100/100=1.0, G=avg(50,50)/100=0.5, B=0, A=1
        let raw = raw_2x2(100, 50, 50, 0);
        let out = QuadBin.to_linear_rgba_f32(&raw);
        assert!((out.pixels[0] - 1.0).abs() < 1e-6);
        assert!((out.pixels[1] - 0.5).abs() < 1e-6);
        assert!((out.pixels[2] - 0.0).abs() < 1e-6);
        assert!((out.pixels[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn quadbin_applies_black_level_and_wb() {
        // black 10 on all; wb R=2.0. R=(100-10)*2/(100-10)=2.0 -> clamps to 1.0.
        let mut raw = raw_2x2(100, 50, 50, 10);
        raw.black_levels = [10.0; 4];
        raw.wb_coeffs = [2.0, 1.0, 1.0, 1.0];
        let out = QuadBin.to_linear_rgba_f32(&raw);
        assert!(
            (out.pixels[0] - 1.0).abs() < 1e-6,
            "R saturates to 1.0 after WB"
        );
        // G=(50-10)/(100-10)=0.444...
        assert!((out.pixels[1] - (40.0 / 90.0)).abs() < 1e-5);
    }

    #[test]
    fn quadbin_parallel_matches_serial_reference_above_threshold() {
        // Sensor large enough that out_w*out_h = 256*256 = 65_536, meeting
        // PARALLEL_MIN_PIXELS, so this exercises the rayon row path.
        let sensor_w: u32 = 512;
        let sensor_h: u32 = 512;
        let mut pixels = vec![0u16; (sensor_w * sensor_h) as usize];
        for y in 0..sensor_h {
            for x in 0..sensor_w {
                // Deterministic, non-trivial pattern that varies per quad
                // position so binning/averaging differences would show up.
                let v = ((x.wrapping_mul(7) + y.wrapping_mul(13) + x * y) % 4001) as u16;
                pixels[(y * sensor_w + x) as usize] = v;
            }
        }
        let raw = crate::raw::RawDecoded {
            width: sensor_w,
            height: sensor_h,
            cpp: 1,
            pixels,
            cfa_pattern: [0, 1, 1, 2], // RGGB
            black_levels: [12.0, 10.0, 11.0, 9.0],
            white_level: 4095.0,
            wb_coeffs: [1.8, 1.0, 1.0, 1.4],
            color_profile: crate::color::ColorProfile::srgb_fallback(),
            orientation: ferrolite_image::Orientation::Normal,
        };

        let out_w = sensor_w / 2;
        let out_h = sensor_h / 2;
        assert!(
            (out_w as u64) * (out_h as u64) >= PARALLEL_MIN_PIXELS,
            "fixture must be above the parallel threshold to exercise the rayon path"
        );

        // Independent serial reference computation — deliberately re-derived
        // here rather than reusing `compute_row`, so a row-indexing /
        // ordering bug in the parallel path would show up as a mismatch.
        let span = (raw.white_level - raw.black_levels[0]).max(1.0);
        let sample_ref = |x: u32, y: u32, quad_idx: usize| -> f32 {
            let (qx, qy) = (quad_idx % 2, quad_idx / 2);
            let px = (x * 2 + qx as u32).min(raw.width - 1);
            let py = (y * 2 + qy as u32).min(raw.height - 1);
            let raw_v = raw.pixels[(py * raw.width + px) as usize] as f32;
            let bl = raw.black_levels[quad_idx];
            ((raw_v - bl) / span).max(0.0)
        };
        let wb = raw.wb_coeffs;
        let mut expected = Vec::with_capacity(LinearRgbaF32::expected_len(out_w, out_h));
        for y in 0..out_h {
            for x in 0..out_w {
                let r = (sample_ref(x, y, 0) * wb[0]).clamp(0.0, 1.0);
                let g =
                    (((sample_ref(x, y, 1) + sample_ref(x, y, 2)) * 0.5) * wb[1]).clamp(0.0, 1.0);
                let b = (sample_ref(x, y, 3) * wb[2]).clamp(0.0, 1.0);
                expected.extend_from_slice(&[r, g, b, 1.0]);
            }
        }

        let out = QuadBin.to_linear_rgba_f32(&raw);
        assert_eq!(out.width, out_w);
        assert_eq!(out.height, out_h);
        assert_eq!(
            out.pixels, expected,
            "parallel output must be bit-identical to the serial reference"
        );
    }
}
