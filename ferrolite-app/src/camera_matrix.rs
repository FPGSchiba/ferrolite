//! Camera→working colour matrix for the current white-balance temperature.
//!
//! P2 Plan 2 (S3): a dual-illuminant `ColorProfile` re-interpolates its
//! camera→working matrix as the WhiteBalance temp changes (Lightroom's model),
//! anchored at D65 and linear in mired. Single-illuminant / fallback profiles
//! reduce to the static matrix (temp only drives the WB uniform, not the
//! matrix). Row-normalized because the RAW demosaic already applied the as-shot
//! neutral gains (see `ferrolite_color::normalize_neutral`).

use ferrolite_color::{
    camera_to_working_interpolated, normalize_neutral, wb_temp_to_cct, Mat3, WorkingSpace, Xy,
};
use ferrolite_decode::ColorProfile;

/// Camera→working 3×3 for `profile` at the normalized WhiteBalance `temp`,
/// row-normalized. Dual-illuminant profiles re-interpolate with `temp` (S3);
/// single-illuminant / fallback profiles are temp-independent (reduce to the
/// static camera→working matrix — the WB uniform still shifts neutrals).
pub fn wb_camera_to_working(profile: &ColorProfile, temp: f32, working: WorkingSpace) -> Mat3 {
    let calibrations: Vec<(Xy, Mat3)> = profile
        .calibrations
        .iter()
        .map(|c| {
            (
                Xy {
                    x: c.white_xy[0],
                    y: c.white_xy[1],
                },
                c.xyz_to_cam,
            )
        })
        .collect();
    let target_cct = wb_temp_to_cct(temp);
    let m = camera_to_working_interpolated(&calibrations, target_cct, working);
    normalize_neutral(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_color::{camera_to_working, mul_vec3};
    use ferrolite_decode::CameraCalibration;

    const M_A: Mat3 = [[1.0, 0.1, 0.0], [0.2, 1.0, 0.1], [0.0, 0.2, 1.0]];
    const M_D65: Mat3 = [[1.2, -0.1, 0.0], [-0.05, 1.1, -0.05], [0.0, -0.1, 1.3]];
    const A_WHITE: [f32; 2] = [0.4476, 0.4074];
    const D65_WHITE: [f32; 2] = [0.3128, 0.3290];

    fn dual_profile() -> ColorProfile {
        ColorProfile {
            xyz_to_cam: M_D65,
            white_xy: D65_WHITE,
            is_fallback: false,
            calibrations: vec![
                CameraCalibration {
                    xyz_to_cam: M_A,
                    white_xy: A_WHITE,
                },
                CameraCalibration {
                    xyz_to_cam: M_D65,
                    white_xy: D65_WHITE,
                },
            ],
        }
    }

    fn single_profile() -> ColorProfile {
        ColorProfile {
            xyz_to_cam: M_D65,
            white_xy: D65_WHITE,
            is_fallback: false,
            calibrations: vec![CameraCalibration {
                xyz_to_cam: M_D65,
                white_xy: D65_WHITE,
            }],
        }
    }

    fn approx_eq(a: &Mat3, b: &Mat3, tol: f32) -> bool {
        (0..3).all(|i| (0..3).all(|j| (a[i][j] - b[i][j]).abs() <= tol))
    }

    #[test]
    fn dual_illuminant_matrix_tracks_temp() {
        let warm = wb_camera_to_working(&dual_profile(), 0.8, WorkingSpace::Rec2020);
        let cool = wb_camera_to_working(&dual_profile(), -0.8, WorkingSpace::Rec2020);
        assert!(
            !approx_eq(&warm, &cool, 1e-4),
            "matrix must change with WB temp"
        );
    }

    #[test]
    fn single_illuminant_matrix_is_temp_independent() {
        let a = wb_camera_to_working(&single_profile(), 0.8, WorkingSpace::Rec2020);
        let b = wb_camera_to_working(&single_profile(), -0.8, WorkingSpace::Rec2020);
        assert!(
            approx_eq(&a, &b, 1e-6),
            "single calibration: temp has no matrix effect"
        );
    }

    #[test]
    fn single_illuminant_equals_legacy_normalize_neutral_path() {
        // Reduces to today's behaviour: normalize_neutral(camera_to_working(...)).
        let got = wb_camera_to_working(&single_profile(), 0.3, WorkingSpace::Rec2020);
        let want = normalize_neutral(camera_to_working(
            M_D65,
            Xy {
                x: D65_WHITE[0],
                y: D65_WHITE[1],
            },
            WorkingSpace::Rec2020,
        ));
        assert!(approx_eq(&got, &want, 1e-6), "got {got:?} want {want:?}");
    }

    #[test]
    fn neutral_stays_neutral_for_any_temp() {
        for &t in &[-1.0_f32, 0.0, 0.7] {
            let m = wb_camera_to_working(&dual_profile(), t, WorkingSpace::Rec2020);
            let out = mul_vec3(&m, &[1.0, 1.0, 1.0]);
            assert!(
                (0..3).all(|i| (out[i] - 1.0).abs() < 1e-4),
                "temp {t}: neutral skewed to {out:?}"
            );
        }
    }

    #[test]
    fn fallback_profile_is_finite() {
        let m = wb_camera_to_working(&ColorProfile::srgb_fallback(), 0.5, WorkingSpace::Rec2020);
        assert!(m.iter().flatten().all(|v: &f32| v.is_finite()));
    }
}
