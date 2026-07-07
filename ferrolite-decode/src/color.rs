//! Camera color calibration surfaced from `rawler` as a decode product.
//!
//! Additive to the existing `{ PreviewImage, RawImage, Metadata }` products
//! (architecture map §3): `ferrolite-pipeline` (Spec 3 Plan 2) feeds this into
//! `ferrolite-color` to build the camera→working matrix. Never panics — a
//! missing/short matrix logs and falls back to sRGB primaries (spec §6, §10).

use rawler::imgop::xyz::{FlatColorMatrix, Illuminant};
use std::collections::HashMap;

/// One camera calibration point: a DNG-style XYZ→camera 3×3 matrix and the
/// CIE 1931 xy white point of the reference illuminant it was calibrated at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraCalibration {
    /// XYZ (reference illuminant) → camera-native linear RGB, row-major 3×3.
    pub xyz_to_cam: [[f32; 3]; 3],
    /// Reference illuminant white point, CIE 1931 xy.
    pub white_xy: [f32; 2],
}

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
    /// All usable camera calibrations (≥1), sorted by white point for a
    /// deterministic order. Additive (architecture map §3): `xyz_to_cam` /
    /// `white_xy` above remain the primary single-matrix view for existing
    /// consumers; new consumers (dual-illuminant interpolation) read this.
    pub calibrations: Vec<CameraCalibration>,
}

impl ColorProfile {
    /// sRGB-primaries fallback (XYZ→sRGB, D65) for cameras lacking a usable
    /// matrix. With an sRGB working space this composes to identity downstream.
    pub fn srgb_fallback() -> Self {
        let xyz_to_cam = [
            [3.2404542, -1.5371385, -0.4985314],
            [-0.969_266, 1.8760108, 0.0415560],
            [0.0556434, -0.2040259, 1.0572252],
        ];
        let white_xy = [0.31271, 0.32902]; // D65
        Self {
            xyz_to_cam,
            white_xy,
            is_fallback: true,
            calibrations: vec![CameraCalibration {
                xyz_to_cam,
                white_xy,
            }],
        }
    }

    /// Build from rawler's per-illuminant color matrices, preferring D65, then
    /// any present matrix. Surfaces every usable calibration in `calibrations`
    /// (additive); falls back to sRGB (logged) when none is usable.
    pub fn from_color_matrix(matrices: &HashMap<Illuminant, FlatColorMatrix>) -> Self {
        // All usable (≥9-element) calibrations, sorted by white point so the
        // order is deterministic regardless of HashMap iteration order.
        let mut calibrations: Vec<CameraCalibration> = matrices
            .iter()
            .filter(|(_, flat)| flat.len() >= 9)
            .map(|(illum, flat)| CameraCalibration {
                xyz_to_cam: reshape_3x3(flat),
                white_xy: illuminant_to_xy(*illum),
            })
            .collect();
        calibrations.sort_by(|a, b| {
            a.white_xy[0]
                .partial_cmp(&b.white_xy[0])
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    a.white_xy[1]
                        .partial_cmp(&b.white_xy[1])
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });

        // Primary single matrix: prefer D65, else any usable matrix (unchanged).
        let picked = matrices
            .get(&Illuminant::D65)
            .filter(|flat| flat.len() >= 9)
            .map(|flat| (Illuminant::D65, flat))
            .or_else(|| {
                matrices
                    .iter()
                    .find(|(_, flat)| flat.len() >= 9)
                    .map(|(illum, flat)| (*illum, flat))
            });

        match picked {
            Some((illum, flat)) => Self {
                xyz_to_cam: reshape_3x3(flat),
                white_xy: illuminant_to_xy(illum),
                is_fallback: false,
                calibrations,
            },
            None => {
                eprintln!("ferrolite-decode: no usable camera color matrix; using sRGB fallback");
                Self::srgb_fallback()
            }
        }
    }
}

/// Reshape a rawler flat color matrix (≥9 elements) into a row-major 3×3.
fn reshape_3x3(flat: &FlatColorMatrix) -> [[f32; 3]; 3] {
    [
        [flat[0], flat[1], flat[2]],
        [flat[3], flat[4], flat[5]],
        [flat[6], flat[7], flat[8]],
    ]
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
        assert_eq!(p.calibrations.len(), 2);
    }

    #[test]
    fn surfaces_both_calibrations_for_dual_illuminant() {
        let mut m: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        m.insert(Illuminant::A, vec![9.0; 9]);
        m.insert(
            Illuminant::D65,
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0],
        );
        let p = ColorProfile::from_color_matrix(&m);
        assert!(!p.is_fallback);
        assert_eq!(p.calibrations.len(), 2, "both A and D65 surfaced");
        // Primary fields unchanged: D65 preferred.
        assert_eq!(
            p.xyz_to_cam,
            [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]]
        );
        assert_eq!(p.white_xy, [0.31271, 0.32902]);
        // Both matrices present among the calibrations.
        let mats: Vec<_> = p.calibrations.iter().map(|c| c.xyz_to_cam).collect();
        assert!(mats.contains(&[[9.0; 3]; 3]));
        assert!(mats.contains(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]]));
    }

    #[test]
    fn single_illuminant_surfaces_one_calibration() {
        let mut m: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        m.insert(Illuminant::A, vec![2.0; 9]);
        let p = ColorProfile::from_color_matrix(&m);
        assert!(!p.is_fallback);
        assert_eq!(p.calibrations.len(), 1);
        assert_eq!(p.calibrations[0].xyz_to_cam, [[2.0; 3]; 3]);
        assert_eq!(p.calibrations[0].white_xy, illuminant_to_xy(Illuminant::A));
    }

    #[test]
    fn fallback_has_one_calibration_matching_primary() {
        let p = ColorProfile::srgb_fallback();
        assert!(p.is_fallback);
        assert_eq!(p.calibrations.len(), 1);
        assert_eq!(p.calibrations[0].xyz_to_cam, p.xyz_to_cam);
        assert_eq!(p.calibrations[0].white_xy, p.white_xy);
    }

    #[test]
    fn empty_map_falls_back_with_one_calibration() {
        let empty: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        let p = ColorProfile::from_color_matrix(&empty);
        assert!(p.is_fallback);
        assert_eq!(p.calibrations.len(), 1);
    }

    #[test]
    fn short_matrix_is_excluded_from_calibrations() {
        let mut m: HashMap<Illuminant, FlatColorMatrix> = HashMap::new();
        m.insert(Illuminant::A, vec![1.0, 2.0, 3.0]); // too short
        m.insert(Illuminant::D65, vec![5.0; 9]); // usable
        let p = ColorProfile::from_color_matrix(&m);
        assert!(!p.is_fallback);
        assert_eq!(p.calibrations.len(), 1, "only the usable D65 matrix");
        assert_eq!(p.calibrations[0].xyz_to_cam, [[5.0; 3]; 3]);
    }

    #[test]
    fn illuminant_to_xy_covers_common_illuminants() {
        assert_eq!(illuminant_to_xy(Illuminant::D50), [0.34567, 0.35850]);
        assert_eq!(illuminant_to_xy(Illuminant::D65), [0.31271, 0.32902]);
        // Unknown illuminants default to D65.
        assert_eq!(illuminant_to_xy(Illuminant::Unknown), [0.31271, 0.32902]);
    }
}
