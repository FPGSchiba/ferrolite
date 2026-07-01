//! sRGB (IEC 61966-2.1) transfer functions. The display/output tail applies the
//! 3×3 matrix (see `tail`) and then one of these; for the sRGB display path this
//! is the only OETF needed in Plan 1.

/// Linear → sRGB-encoded.
pub fn srgb_oetf(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB-encoded → linear.
pub fn srgb_eotf(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_fixed() {
        assert!((srgb_oetf(0.0) - 0.0).abs() < 1e-6);
        assert!((srgb_oetf(1.0) - 1.0).abs() < 1e-5);
        assert!((srgb_eotf(0.0) - 0.0).abs() < 1e-6);
        assert!((srgb_eotf(1.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn oetf_eotf_round_trip() {
        for i in 0..=20 {
            let l = i as f32 / 20.0;
            let round = srgb_eotf(srgb_oetf(l));
            assert!((round - l).abs() < 1e-4, "l={l} round={round}");
        }
    }

    #[test]
    fn linear_segment_near_zero() {
        // Below the knee the curve is exactly 12.92 * linear.
        assert!((srgb_oetf(0.002) - 12.92 * 0.002).abs() < 1e-6);
    }
}
