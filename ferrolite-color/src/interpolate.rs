//! Dual-illuminant camera→working interpolation (DNG-style, mired-weighted).
//!
//! Linearly blends the two DNG `ColorMatrix` (`xyz_to_cam`) calibrations by
//! inverse CCT (mired) — DNG's convention — then composes the camera→working
//! transform via [`camera_to_working`] using the target illuminant's white
//! point. A single calibration reduces exactly to [`camera_to_working`]; none
//! degrades to identity (decode always supplies ≥1). Pure, `unsafe`-free.

use crate::camera::camera_to_working;
use crate::cct::{cct_to_xy, xy_to_cct};
use crate::matrix::{identity, Mat3, Xy};
use crate::working_space::WorkingSpace;

/// Interpolated camera→working matrix following the white-balance temperature.
///
/// `calibrations` are the camera's DNG calibration points as
/// `(reference_white_xy, xyz_to_cam)` pairs (from `ColorProfile::calibrations`);
/// `target_cct` is the scene / white-balance colour temperature (Kelvin).
pub fn camera_to_working_interpolated(
    calibrations: &[(Xy, Mat3)],
    target_cct: f32,
    working: WorkingSpace,
) -> Mat3 {
    match interpolate_xyz_to_cam(calibrations, target_cct) {
        Some((xyz_to_cam, cam_white)) => camera_to_working(xyz_to_cam, cam_white, working),
        None => identity(),
    }
}

/// Blend the calibrations' `xyz_to_cam` for `target_cct`, returning the matrix
/// and its reference white. `None` when there are no calibrations.
///
/// * 1 calibration  → that matrix + its own white (reduces to `camera_to_working`).
/// * ≥2 calibrations → linear blend of the lowest- and highest-CCT matrices,
///   weighted by inverse CCT (mired), with the target illuminant's white
///   `cct_to_xy(target_cct)`.
pub(crate) fn interpolate_xyz_to_cam(
    calibrations: &[(Xy, Mat3)],
    target_cct: f32,
) -> Option<(Mat3, Xy)> {
    match calibrations.len() {
        0 => None,
        1 => Some((calibrations[0].1, calibrations[0].0)),
        _ => {
            // Order by CCT so the low/high endpoints are well-defined and the
            // result is independent of the input order.
            let mut by_cct: Vec<(f32, Mat3)> = calibrations
                .iter()
                .map(|(white, m)| (xy_to_cct(*white), *m))
                .collect();
            by_cct.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let (cct_lo, m_lo) = by_cct[0];
            let (cct_hi, m_hi) = by_cct[by_cct.len() - 1];
            let f = mired_weight(target_cct, cct_lo, cct_hi);
            Some((lerp_mat3(&m_lo, &m_hi, f), cct_to_xy(target_cct)))
        }
    }
}

/// DNG mired interpolation weight toward the high-CCT endpoint, clamped [0,1]:
/// `f = (1/target − 1/cct_lo) / (1/cct_hi − 1/cct_lo)`.
fn mired_weight(target_cct: f32, cct_lo: f32, cct_hi: f32) -> f32 {
    let denom = 1.0 / cct_hi - 1.0 / cct_lo;
    if denom.abs() < f32::EPSILON {
        return 0.0;
    }
    ((1.0 / target_cct - 1.0 / cct_lo) / denom).clamp(0.0, 1.0)
}

/// Element-wise linear blend `(1 − f)·a + f·b`.
#[allow(clippy::needless_range_loop)] // explicit i/j indexing is clearest for a fixed 3×3.
fn lerp_mat3(a: &Mat3, b: &Mat3, f: f32) -> Mat3 {
    let mut out = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = a[i][j] * (1.0 - f) + b[i][j] * f;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::approx_eq_mat3;

    const A_WHITE: Xy = Xy {
        x: 0.4476,
        y: 0.4074,
    };
    const D65_WHITE: Xy = Xy {
        x: 0.3128,
        y: 0.3290,
    };

    // Two visibly distinct fake calibration matrices.
    const M_A: Mat3 = [[1.0, 0.1, 0.0], [0.2, 1.0, 0.1], [0.0, 0.2, 1.0]];
    const M_D65: Mat3 = [[1.5, -0.2, 0.0], [-0.1, 1.4, -0.1], [0.0, -0.3, 1.6]];

    #[test]
    fn zero_calibrations_is_identity() {
        let m = camera_to_working_interpolated(&[], 5000.0, WorkingSpace::Rec2020);
        assert!(approx_eq_mat3(&m, &identity(), 1e-6));
    }

    #[test]
    fn single_calibration_equals_camera_to_working() {
        let cal = [(D65_WHITE, M_D65)];
        let got = camera_to_working_interpolated(&cal, 5000.0, WorkingSpace::Rec2020);
        let want = camera_to_working(M_D65, D65_WHITE, WorkingSpace::Rec2020);
        assert!(
            approx_eq_mat3(&got, &want, 1e-6),
            "got {got:?} want {want:?}"
        );
    }

    #[test]
    fn blend_at_low_endpoint_selects_low_matrix() {
        let cals = [(A_WHITE, M_A), (D65_WHITE, M_D65)];
        let (m, _white) =
            interpolate_xyz_to_cam(&cals, xy_to_cct(A_WHITE)).expect("two calibrations");
        assert!(
            approx_eq_mat3(&m, &M_A, 1e-6),
            "at A expected M_A, got {m:?}"
        );
    }

    #[test]
    fn blend_at_high_endpoint_selects_high_matrix() {
        let cals = [(A_WHITE, M_A), (D65_WHITE, M_D65)];
        let (m, _white) =
            interpolate_xyz_to_cam(&cals, xy_to_cct(D65_WHITE)).expect("two calibrations");
        assert!(
            approx_eq_mat3(&m, &M_D65, 1e-6),
            "at D65 expected M_D65, got {m:?}"
        );
    }

    #[test]
    fn blend_midpoint_is_between_endpoints() {
        let cals = [(A_WHITE, M_A), (D65_WHITE, M_D65)];
        let mid_mired = 0.5 * (1.0 / xy_to_cct(A_WHITE) + 1.0 / xy_to_cct(D65_WHITE));
        let mid_cct = 1.0 / mid_mired;
        let (m, _white) = interpolate_xyz_to_cam(&cals, mid_cct).expect("two calibrations");
        // Element [0][0] must sit strictly between 1.0 and 1.5.
        assert!(
            m[0][0] > 1.0 && m[0][0] < 1.5,
            "midpoint [0][0]={}",
            m[0][0]
        );
        // At the mired midpoint the blend weight is 0.5, so it is the average.
        let avg = 0.5 * (M_A[0][0] + M_D65[0][0]);
        assert!(
            (m[0][0] - avg).abs() < 1e-4,
            "expected avg {avg}, got {}",
            m[0][0]
        );
    }

    #[test]
    fn calibration_order_does_not_matter() {
        let forward = [(A_WHITE, M_A), (D65_WHITE, M_D65)];
        let reversed = [(D65_WHITE, M_D65), (A_WHITE, M_A)];
        let a = camera_to_working_interpolated(&forward, 4000.0, WorkingSpace::Rec2020);
        let b = camera_to_working_interpolated(&reversed, 4000.0, WorkingSpace::Rec2020);
        assert!(approx_eq_mat3(&a, &b, 1e-6));
    }

    #[test]
    fn output_is_finite_for_all_working_spaces() {
        let cals = [(A_WHITE, M_A), (D65_WHITE, M_D65)];
        for space in WorkingSpace::ALL {
            let m = camera_to_working_interpolated(&cals, 5000.0, space);
            assert!(m.iter().flatten().all(|v: &f32| v.is_finite()), "{space:?}");
        }
    }
}
