//! Bradford chromatic adaptation between two white points.

use crate::matrix::{diag, inverse, mul_mat3, mul_vec3, Mat3, Xy};

/// The Bradford cone-response matrix (XYZ → LMS-ish cone space).
const BRADFORD: Mat3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// XYZ→XYZ matrix adapting a color measured under white point `src` to its
/// appearance under white point `dst` (Bradford transform).
pub fn chromatic_adaptation(src: Xy, dst: Xy) -> Mat3 {
    let cone_src = mul_vec3(&BRADFORD, &src.to_xyz());
    let cone_dst = mul_vec3(&BRADFORD, &dst.to_xyz());
    let ratio = [
        cone_dst[0] / cone_src[0],
        cone_dst[1] / cone_src[1],
        cone_dst[2] / cone_src[2],
    ];
    let b_inv = inverse(&BRADFORD).expect("Bradford matrix is invertible");
    mul_mat3(&b_inv, &mul_mat3(&diag(&ratio), &BRADFORD))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::{approx_eq_mat3, identity, mul_mat3, mul_vec3, Xy};

    const D65: Xy = Xy {
        x: 0.31271,
        y: 0.32902,
    };
    const D50: Xy = Xy {
        x: 0.34567,
        y: 0.35850,
    };

    #[test]
    fn same_white_is_identity() {
        assert!(approx_eq_mat3(
            &chromatic_adaptation(D65, D65),
            &identity(),
            1e-5
        ));
    }

    #[test]
    fn adaptation_is_invertible_round_trip() {
        let there = chromatic_adaptation(D50, D65);
        let back = chromatic_adaptation(D65, D50);
        assert!(approx_eq_mat3(&mul_mat3(&back, &there), &identity(), 1e-4));
    }

    #[test]
    fn maps_source_white_onto_destination_white() {
        // Adapting src-white XYZ must yield dst-white XYZ.
        let a = chromatic_adaptation(D50, D65);
        let got = mul_vec3(&a, &D50.to_xyz());
        let want = D65.to_xyz();
        assert!(
            (0..3).all(|i| (got[i] - want[i]).abs() < 1e-4),
            "got {got:?} want {want:?}"
        );
    }
}
