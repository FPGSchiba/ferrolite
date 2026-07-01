//! Camera-native RGB → working-space RGB, composed as a single 3×3.

use crate::adapt::chromatic_adaptation;
use crate::matrix::{identity, inverse, mul_mat3, Mat3, Xy};
use crate::working_space::WorkingSpace;

/// Compose `xyz_to_working · adapt(cam_white → working_white) · cam_to_xyz`.
///
/// `xyz_to_cam` is the DNG-style XYZ→camera matrix (as surfaced by
/// `ferrolite-decode`'s `ColorProfile`); `cam_white` is the matrix's reference
/// illuminant white point. Pragmatic single-illuminant transform (spec §4.2);
/// quality is secondary to architecture. A singular `xyz_to_cam` degrades to an
/// identity camera→XYZ rather than panicking.
pub fn camera_to_working(xyz_to_cam: Mat3, cam_white: Xy, working: WorkingSpace) -> Mat3 {
    let cam_to_xyz = inverse(&xyz_to_cam).unwrap_or_else(identity);
    let adapt = chromatic_adaptation(cam_white, working.white_point());
    let xyz_to_working = working.xyz_to_rgb();
    mul_mat3(&xyz_to_working, &mul_mat3(&adapt, &cam_to_xyz))
}

/// Row-normalize a camera→working matrix so each row sums to 1, making a
/// white-balanced neutral (1,1,1) map to a working-space neutral (1,1,1).
///
/// Apply this ONLY when the camera samples have already been white-balanced by
/// the as-shot neutral gains (as the RAW demosaic does): the DNG `ColorMatrix`
/// that `camera_to_working` is built from independently neutralizes the camera's
/// native response, so without this the white balance is effectively applied
/// twice and neutrals skew (typically red). This is the dcraw/libraw camera-to-
/// output convention. Do NOT apply it to an already-neutral source (e.g. an sRGB
/// preview), whose transform legitimately has non-unit row sums.
///
/// A row summing to ~0 is left unscaled rather than producing non-finite values.
pub fn normalize_neutral(m: Mat3) -> Mat3 {
    let mut out = m;
    for row in out.iter_mut() {
        let sum = row[0] + row[1] + row[2];
        if sum.abs() > 1e-6 {
            row[0] /= sum;
            row[1] /= sum;
            row[2] /= sum;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::matrix::{approx_eq_mat3, Mat3, Xy};
    use crate::working_space::WorkingSpace;

    const D65: Xy = Xy {
        x: 0.31271_f32,
        y: 0.32902_f32,
    };

    // rawler-0.7.2/src/imgop/xyz.rs XYZ_TO_SRGB_D65 — an "sRGB camera".
    #[allow(clippy::excessive_precision)]
    const XYZ_TO_SRGB_D65: Mat3 = [
        [3.2404542, -1.5371385, -0.4985314],
        [-0.9692660, 1.8760108, 0.0415560],
        [0.0556434, -0.2040259, 1.0572252],
    ];

    #[test]
    fn srgb_camera_into_srgb_working_is_identity() {
        // A camera whose XYZ→cam == XYZ→sRGB, into the sRGB working space
        // under the same white, must reduce to identity.
        let m = super::camera_to_working(XYZ_TO_SRGB_D65, D65, WorkingSpace::Srgb);
        assert!(
            approx_eq_mat3(&m, &crate::matrix::identity(), 1e-3),
            "{m:?}"
        );
    }

    #[test]
    fn output_is_finite_for_all_working_spaces() {
        for space in WorkingSpace::ALL {
            let m = super::camera_to_working(XYZ_TO_SRGB_D65, D65, space);
            assert!(
                m.iter().flatten().all(|v: &f32| v.is_finite()),
                "{space:?} produced non-finite"
            );
        }
    }

    #[test]
    fn singular_matrix_does_not_panic() {
        let singular: Mat3 = [[0.0; 3]; 3];
        let m = super::camera_to_working(singular, D65, WorkingSpace::Rec2020);
        assert!(m.iter().flatten().all(|v: &f32| v.is_finite()));
    }

    #[test]
    fn normalize_neutral_maps_neutral_to_neutral() {
        // A matrix whose rows sum to != 1 skews a white-balanced neutral; after
        // row-normalization, (1,1,1) maps to (1,1,1).
        let m: Mat3 = [
            [3.125, -0.067, -0.174],
            [0.075, 1.267, -0.458],
            [0.149, -0.268, 1.410],
        ];
        let n = super::normalize_neutral(m);
        let out = crate::matrix::mul_vec3(&n, &[1.0, 1.0, 1.0]);
        assert!(
            (0..3).all(|i| (out[i] - 1.0).abs() < 1e-5),
            "neutral should stay neutral, got {out:?}"
        );
    }

    #[test]
    fn normalize_neutral_leaves_zero_row_unscaled() {
        let m: Mat3 = [[1.0, -0.5, -0.5], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
        let n = super::normalize_neutral(m);
        assert_eq!(n[1], [0.0, 0.0, 0.0], "zero-sum row is left as-is");
        assert!(n.iter().flatten().all(|v: &f32| v.is_finite()));
    }
}
