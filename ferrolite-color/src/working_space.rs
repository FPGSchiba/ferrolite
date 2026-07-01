//! The curated 5 working spaces and their linear RGB↔XYZ matrices, computed
//! from primaries + white point (Bruce Lindbloom's method).

use crate::matrix::{diag, inverse, mul_mat3, mul_vec3, Mat3, Xy};

/// The curated working/output color spaces. Default = linear Rec.2020.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum WorkingSpace {
    Srgb,
    AdobeRgb,
    DisplayP3,
    #[default]
    Rec2020,
    ProPhoto,
}

impl WorkingSpace {
    /// All five spaces, for iteration in tests and UI selectors.
    pub const ALL: [WorkingSpace; 5] = [
        WorkingSpace::Srgb,
        WorkingSpace::AdobeRgb,
        WorkingSpace::DisplayP3,
        WorkingSpace::Rec2020,
        WorkingSpace::ProPhoto,
    ];

    /// RGB primaries (R, G, B) and the reference white point, all as CIE xy.
    fn primaries(&self) -> ([Xy; 3], Xy) {
        let d65 = Xy {
            x: 0.31271,
            y: 0.32902,
        };
        let d50 = Xy {
            x: 0.34567,
            y: 0.35850,
        };
        match self {
            WorkingSpace::Srgb => (
                [
                    Xy { x: 0.640, y: 0.330 },
                    Xy { x: 0.300, y: 0.600 },
                    Xy { x: 0.150, y: 0.060 },
                ],
                d65,
            ),
            WorkingSpace::AdobeRgb => (
                [
                    Xy { x: 0.640, y: 0.330 },
                    Xy { x: 0.210, y: 0.710 },
                    Xy { x: 0.150, y: 0.060 },
                ],
                d65,
            ),
            WorkingSpace::DisplayP3 => (
                [
                    Xy { x: 0.680, y: 0.320 },
                    Xy { x: 0.265, y: 0.690 },
                    Xy { x: 0.150, y: 0.060 },
                ],
                d65,
            ),
            WorkingSpace::Rec2020 => (
                [
                    Xy { x: 0.708, y: 0.292 },
                    Xy { x: 0.170, y: 0.797 },
                    Xy { x: 0.131, y: 0.046 },
                ],
                d65,
            ),
            WorkingSpace::ProPhoto => (
                [
                    Xy {
                        x: 0.7347,
                        y: 0.2653,
                    },
                    Xy {
                        x: 0.1596,
                        y: 0.8404,
                    },
                    Xy {
                        x: 0.0366,
                        y: 0.0001,
                    },
                ],
                d50,
            ),
        }
    }

    /// The space's reference white point (CIE xy).
    pub fn white_point(&self) -> Xy {
        self.primaries().1
    }

    /// Linear RGB → XYZ (under this space's own white point).
    pub fn rgb_to_xyz(&self) -> Mat3 {
        let (p, white) = self.primaries();
        let (xr, xg, xb) = (p[0].to_xyz(), p[1].to_xyz(), p[2].to_xyz());
        // Columns are the primary tristimulus values.
        let m: Mat3 = [
            [xr[0], xg[0], xb[0]],
            [xr[1], xg[1], xb[1]],
            [xr[2], xg[2], xb[2]],
        ];
        let s = mul_vec3(
            &inverse(&m).expect("primaries are linearly independent"),
            &white.to_xyz(),
        );
        mul_mat3(&m, &diag(&s))
    }

    /// XYZ → linear RGB (under this space's own white point).
    pub fn xyz_to_rgb(&self) -> Mat3 {
        inverse(&self.rgb_to_xyz()).expect("rgb_to_xyz is invertible")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::{approx_eq_mat3, identity, mul_mat3};

    // From rawler-0.7.2/src/imgop/xyz.rs SRGB_TO_XYZ_D65 (Bruce Lindbloom).
    const SRGB_TO_XYZ_D65: Mat3 = [
        [0.4124564, 0.3575761, 0.1804375],
        [0.2126729, 0.7151522, 0.0721750],
        [0.0193339, 0.119_192, 0.9503041],
    ];

    #[test]
    fn default_is_rec2020() {
        assert_eq!(WorkingSpace::default(), WorkingSpace::Rec2020);
    }

    #[test]
    fn srgb_rgb_to_xyz_matches_reference() {
        // Computed from primaries+white; must match the published matrix.
        assert!(approx_eq_mat3(
            &WorkingSpace::Srgb.rgb_to_xyz(),
            &SRGB_TO_XYZ_D65,
            1e-3
        ));
    }

    #[test]
    fn every_space_rgb_to_xyz_inverts_cleanly() {
        for space in WorkingSpace::ALL {
            let round = mul_mat3(&space.xyz_to_rgb(), &space.rgb_to_xyz());
            assert!(
                approx_eq_mat3(&round, &identity(), 1e-4),
                "{space:?} rgb_to_xyz/xyz_to_rgb not inverse"
            );
        }
    }

    #[test]
    fn white_maps_to_white_point_xyz() {
        // rgb_to_xyz * (1,1,1) == white point XYZ (definition of the adaptation).
        for space in WorkingSpace::ALL {
            let got = crate::matrix::mul_vec3(&space.rgb_to_xyz(), &[1.0, 1.0, 1.0]);
            let want = space.white_point().to_xyz();
            assert!(
                (0..3).all(|i| (got[i] - want[i]).abs() < 1e-4),
                "{space:?} white mismatch: got {got:?} want {want:?}"
            );
        }
    }
}
