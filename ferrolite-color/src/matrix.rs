//! Tiny 3×3 linear-algebra core — generic, no color concepts.

/// Row-major 3×3 matrix.
pub type Mat3 = [[f32; 3]; 3];

/// A CIE 1931 xy chromaticity coordinate (a white point or primary).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xy {
    pub x: f32,
    pub y: f32,
}

impl Xy {
    /// Chromaticity → tristimulus XYZ, normalized so Y = 1.
    pub fn to_xyz(&self) -> [f32; 3] {
        [self.x / self.y, 1.0, (1.0 - self.x - self.y) / self.y]
    }
}

/// The 3×3 identity.
pub fn identity() -> Mat3 {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
}

/// Matrix product `a · b`.
#[allow(clippy::needless_range_loop)] // explicit i/j/k indexing is clearest for a fixed 3×3.
pub fn mul_mat3(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut r = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    r
}

/// Matrix–vector product `a · v`.
pub fn mul_vec3(a: &Mat3, v: &[f32; 3]) -> [f32; 3] {
    [
        a[0][0] * v[0] + a[0][1] * v[1] + a[0][2] * v[2],
        a[1][0] * v[0] + a[1][1] * v[1] + a[1][2] * v[2],
        a[2][0] * v[0] + a[2][1] * v[1] + a[2][2] * v[2],
    ]
}

/// Diagonal matrix from a 3-vector.
pub fn diag(v: &[f32; 3]) -> Mat3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}

/// Inverse via cofactors; `None` when (near-)singular.
pub fn inverse(m: &Mat3) -> Option<Mat3> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-12 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ])
}

/// Test helper: element-wise closeness within `tol`.
#[cfg(test)]
pub(crate) fn approx_eq_mat3(a: &Mat3, b: &Mat3, tol: f32) -> bool {
    (0..3).all(|i| (0..3).all(|j| (a[i][j] - b[i][j]).abs() <= tol))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_multiplicative_unit() {
        let m: Mat3 = [[2.0, 0.0, 1.0], [0.0, 3.0, 0.0], [4.0, 0.0, 5.0]];
        assert!(approx_eq_mat3(&mul_mat3(&identity(), &m), &m, 1e-6));
        assert!(approx_eq_mat3(&mul_mat3(&m, &identity()), &m, 1e-6));
    }

    #[test]
    fn inverse_round_trips_to_identity() {
        let m: Mat3 = [[2.0, 0.0, 1.0], [0.0, 3.0, 0.0], [4.0, 0.0, 5.0]];
        let inv = inverse(&m).expect("m is invertible");
        assert!(approx_eq_mat3(&mul_mat3(&m, &inv), &identity(), 1e-5));
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        let singular: Mat3 = [[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 0.0, 0.0]];
        assert!(inverse(&singular).is_none());
    }

    #[test]
    fn mul_vec3_matches_hand_computation() {
        let m: Mat3 = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]];
        assert_eq!(mul_vec3(&m, &[1.0, 0.0, -1.0]), [-2.0, -2.0, -2.0]);
    }

    #[test]
    fn xy_to_xyz_normalizes_luminance_to_one() {
        let xyz = (Xy {
            x: 0.3127,
            y: 0.3290,
        })
        .to_xyz();
        assert!((xyz[1] - 1.0).abs() < 1e-6);
        assert!((xyz[0] - 0.3127 / 0.3290).abs() < 1e-5);
    }
}
