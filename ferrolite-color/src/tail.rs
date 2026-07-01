//! Tail transforms: working-space RGB → display or output RGB (the 3×3 matrix;
//! the OETF is applied separately, in-shader for display / at encode for output).

use crate::adapt::chromatic_adaptation;
use crate::matrix::{mul_mat3, Mat3};
use crate::working_space::WorkingSpace;

/// `working` linear RGB → `output` linear RGB, as a single 3×3.
pub fn working_to_output(working: WorkingSpace, output: WorkingSpace) -> Mat3 {
    let adapt = chromatic_adaptation(working.white_point(), output.white_point());
    mul_mat3(
        &output.xyz_to_rgb(),
        &mul_mat3(&adapt, &working.rgb_to_xyz()),
    )
}

/// `working` linear RGB → sRGB (D65) display linear RGB, as a single 3×3.
/// The sRGB OETF is applied after this matrix (in-shader). With
/// `WorkingSpace::Srgb` this is exactly the identity (spec §4.3 invariant).
pub fn working_to_display(working: WorkingSpace) -> Mat3 {
    working_to_output(working, WorkingSpace::Srgb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::{approx_eq_mat3, identity};
    use crate::working_space::WorkingSpace;

    #[test]
    fn srgb_working_to_display_is_identity() {
        // The regression invariant (spec §4.3): with sRGB working space the tail
        // matrix is identity, so the shader reduces to plain sRGB OETF.
        assert!(approx_eq_mat3(
            &working_to_display(WorkingSpace::Srgb),
            &identity(),
            1e-4
        ));
    }

    #[test]
    fn output_to_same_space_is_identity() {
        for space in WorkingSpace::ALL {
            assert!(
                approx_eq_mat3(&working_to_output(space, space), &identity(), 1e-4),
                "{space:?} -> {space:?} not identity"
            );
        }
    }

    #[test]
    fn all_tails_are_finite() {
        for space in WorkingSpace::ALL {
            assert!(working_to_display(space)
                .iter()
                .flatten()
                .all(|v: &f32| v.is_finite()));
        }
    }
}
