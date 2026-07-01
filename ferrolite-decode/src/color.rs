//! Camera color calibration surfaced from `rawler` as a decode product.
//!
//! Additive to the existing `{ PreviewImage, RawImage, Metadata }` products
//! (architecture map §3): `ferrolite-pipeline` (Spec 3 Plan 2) feeds this into
//! `ferrolite-color` to build the camera→working matrix. Never panics — a
//! missing/short matrix logs and falls back to sRGB primaries (spec §6, §10).

use rawler::imgop::xyz::{FlatColorMatrix, Illuminant};
use std::collections::HashMap;

/// Camera color calibration: the DNG-style XYZ→camera 3×3 matrix and the
/// reference illuminant it was calibrated for.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorProfile {
    /// XYZ (reference illuminant) → camera-native linear RGB, row-major 3×3
    /// (DNG `ColorMatrix` convention, as provided by rawler).
    pub xyz_to_cam: [[f32; 3]; 3],
    /// Reference illuminant white point, CIE 1931 xy.
    pub white_xy: [f32; 2],
    /// True when this is the synthetic sRGB fallback (no usable camera matrix).
    pub is_fallback: bool,
}

impl ColorProfile {
    /// sRGB-primaries fallback (XYZ→sRGB, D65) for cameras lacking a usable
    /// matrix. With an sRGB working space this composes to identity downstream.
    pub fn srgb_fallback() -> Self {
        Self {
            xyz_to_cam: [
                [3.2404542, -1.5371385, -0.4985314],
                [-0.969_266, 1.8760108, 0.0415560],
                [0.0556434, -0.2040259, 1.0572252],
            ],
            white_xy: [0.31271, 0.32902], // D65
            is_fallback: true,
        }
    }

    /// Build from rawler's per-illuminant color matrices, preferring D65, then
    /// any present matrix. Falls back to sRGB (logged) when none is usable.
    pub fn from_color_matrix(matrices: &HashMap<Illuminant, FlatColorMatrix>) -> Self {
        let picked = matrices
            .get(&Illuminant::D65)
            .map(|flat| (Illuminant::D65, flat))
            .or_else(|| matrices.iter().next().map(|(illum, flat)| (*illum, flat)));

        match picked {
            Some((illum, flat)) if flat.len() >= 9 => Self {
                xyz_to_cam: [
                    [flat[0], flat[1], flat[2]],
                    [flat[3], flat[4], flat[5]],
                    [flat[6], flat[7], flat[8]],
                ],
                white_xy: illuminant_to_xy(illum),
                is_fallback: false,
            },
            _ => {
                eprintln!("ferrolite-decode: no usable camera color matrix; using sRGB fallback");
                Self::srgb_fallback()
            }
        }
    }
}

/// Map a rawler illuminant to a CIE 1931 xy white point. Unknown → D65.
pub fn illuminant_to_xy(illum: Illuminant) -> [f32; 2] {
    match illum {
        Illuminant::D50 => [0.34567, 0.35850],
        Illuminant::D55 => [0.33242, 0.34743],
        Illuminant::D75 => [0.29902, 0.31485],
        Illuminant::A | Illuminant::Tungsten => [0.44757, 0.40745],
        Illuminant::B => [0.34842, 0.35161],
        Illuminant::C => [0.31006, 0.31616],
        // D65 and daylight-like illuminants (and anything unmapped) → D65.
        _ => [0.31271, 0.32902],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn srgb_fallback_is_flagged_d65() {
        let p = ColorProfile::srgb_fallback();
        assert!(p.is_fallback);
        assert_eq!(p.white_xy, [0.31271, 0.32902]);
        // First row of XYZ->sRGB(D65).
        assert!((p.xyz_to_cam[0][0] - 3.2404542).abs() < 1e-5);
    }

    #[test]
    fn empty_matrix_map_falls_back() {
        let empty: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        let p = ColorProfile::from_color_matrix(&empty);
        assert!(p.is_fallback);
    }

    #[test]
    fn too_short_matrix_falls_back() {
        let mut m: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        m.insert(Illuminant::D65, vec![1.0, 0.0, 0.0]); // only 3 values
        let p = ColorProfile::from_color_matrix(&m);
        assert!(p.is_fallback);
    }

    #[test]
    fn prefers_d65_and_reshapes_to_3x3() {
        let mut m: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        m.insert(Illuminant::A, vec![9.0; 9]);
        m.insert(
            Illuminant::D65,
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0],
        );
        let p = ColorProfile::from_color_matrix(&m);
        assert!(!p.is_fallback);
        assert_eq!(
            p.xyz_to_cam,
            [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]]
        );
        assert_eq!(p.white_xy, [0.31271, 0.32902]);
    }

    #[test]
    fn illuminant_to_xy_covers_common_illuminants() {
        assert_eq!(illuminant_to_xy(Illuminant::D50), [0.34567, 0.35850]);
        assert_eq!(illuminant_to_xy(Illuminant::D65), [0.31271, 0.32902]);
        // Unknown illuminants default to D65.
        assert_eq!(illuminant_to_xy(Illuminant::Unknown), [0.31271, 0.32902]);
    }
}
