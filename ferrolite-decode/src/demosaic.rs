//! CFA → display-linear RGBA conversion. Photo-domain (needs WB / black level),
//! so it lives here, not in the engine tier. `QuadBin` is the fast half-res
//! default; a full-res `Bilinear` impl is a future drop-in behind the trait.

use crate::raw::RawDecoded;
use ferrolite_image::LinearRgbaF32;

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
        let mut pixels = Vec::with_capacity(LinearRgbaF32::expected_len(out_w, out_h));
        for y in 0..out_h {
            for x in 0..out_w {
                let r = (sample(x, y, r_pos) * wb[0]).clamp(0.0, 1.0);
                let g = (((sample(x, y, g0) + sample(x, y, g1)) * 0.5) * wb[1]).clamp(0.0, 1.0);
                let b = (sample(x, y, b_pos) * wb[2]).clamp(0.0, 1.0);
                pixels.extend_from_slice(&[r, g, b, 1.0]);
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
}
