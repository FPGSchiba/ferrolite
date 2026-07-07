//! Pure per-pixel output conversion: working-linear RGB → output-space encoded
//! RGB. The 3×3 (working→output) and the output OETF both come from
//! ferrolite-color (spec §8.1). No GPU, fully unit-tested.

use ferrolite_color::{mul_vec3, output_oetf, Mat3, WorkingSpace};

/// Apply the working→output 3×3, clamp to `[0,1]`, then the output OETF.
pub(crate) fn convert_pixel(rgb_lin: [f32; 3], m: &Mat3, out: WorkingSpace) -> [f32; 3] {
    let lin = mul_vec3(m, &rgb_lin);
    // Unclamped working values must never encode a NaN pixel (§6): map NaN to a
    // defined 0.0 before the OETF. ±Inf is left to `output_oetf`'s clamp (→ 1/0).
    let nz = |v: f32| if v.is_nan() { 0.0 } else { v };
    [
        output_oetf(out, nz(lin[0])),
        output_oetf(out, nz(lin[1])),
        output_oetf(out, nz(lin[2])),
    ]
}

/// Quantize an encoded (0..1) RGB triple to 8-bit, rounding + clamping.
pub(crate) fn to_u8(encoded: [f32; 3]) -> [u8; 3] {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [q(encoded[0]), q(encoded[1]), q(encoded[2])]
}

/// Quantize an encoded (0..1) RGB triple to 16-bit, rounding + clamping.
pub(crate) fn to_u16(encoded: [f32; 3]) -> [u16; 3] {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    [q(encoded[0]), q(encoded[1]), q(encoded[2])]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_color::{identity, srgb_oetf, WorkingSpace};

    #[test]
    fn identity_matrix_srgb_is_just_oetf() {
        let m = identity(); // working==output==sRGB -> working_to_output is identity
        let out = convert_pixel([0.5, 0.25, 0.0], &m, WorkingSpace::Srgb);
        assert!((out[0] - srgb_oetf(0.5)).abs() < 1e-5);
        assert!((out[1] - srgb_oetf(0.25)).abs() < 1e-5);
        assert!((out[2] - srgb_oetf(0.0)).abs() < 1e-5);
    }

    #[test]
    fn clamps_out_of_gamut_before_oetf() {
        let m = identity();
        // A negative and a >1 channel clamp to [0,1] endpoints.
        let out = convert_pixel([-0.2, 2.0, 0.5], &m, WorkingSpace::Srgb);
        assert_eq!(out[0], 0.0);
        assert!((out[1] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn quantizers_round_and_clamp() {
        assert_eq!(to_u8([0.0, 1.0, 0.5]), [0, 255, 128]);
        assert_eq!(to_u8([-1.0, 2.0, 0.5]), [0, 255, 128]);
        assert_eq!(to_u16([0.0, 1.0, 0.5]), [0, 65535, 32768]);
    }

    #[test]
    fn sanitizes_non_finite_channels() {
        let m = identity();
        // Isolate each non-finite input: the identity dot product would otherwise
        // cross-contaminate via 0*Inf = NaN, so drive one channel at a time (the
        // identity diagonal passes it through; the others become NaN → masked to 0).
        // NaN -> 0 (nz guard); +Inf -> white and -Inf -> black (output_oetf clamp).
        let nan_ch = convert_pixel([f32::NAN, 0.0, 0.0], &m, WorkingSpace::Srgb);
        assert!(
            nan_ch.iter().all(|v| v.is_finite()),
            "NaN must sanitize, got {nan_ch:?}"
        );
        assert_eq!(to_u8(nan_ch)[0], 0, "NaN channel -> 0");

        let pos_inf = convert_pixel([0.0, f32::INFINITY, 0.0], &m, WorkingSpace::Srgb);
        assert!(
            pos_inf.iter().all(|v| v.is_finite()),
            "output must be finite, got {pos_inf:?}"
        );
        assert_eq!(
            to_u8(pos_inf)[1],
            255,
            "+Inf channel clamps to white via output_oetf"
        );

        let neg_inf = convert_pixel([0.0, 0.0, f32::NEG_INFINITY], &m, WorkingSpace::Srgb);
        assert!(
            neg_inf.iter().all(|v| v.is_finite()),
            "output must be finite, got {neg_inf:?}"
        );
        assert_eq!(
            to_u8(neg_inf)[2],
            0,
            "-Inf channel clamps to black via output_oetf"
        );
    }
}
