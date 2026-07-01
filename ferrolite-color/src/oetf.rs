//! sRGB (IEC 61966-2.1) transfer functions. The display/output tail applies the
//! 3×3 matrix (see `tail`) and then one of these; for the sRGB display path this
//! is the only OETF needed in Plan 1.

use crate::WorkingSpace;

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

/// Encode a linear working-space channel into the given output space's encoded
/// (display-referred) value. Input is clamped to `[0, 1]`. Used at export encode
/// time (spec §8.1); the display path uses `srgb_oetf` in-shader instead.
pub fn output_oetf(space: WorkingSpace, linear: f32) -> f32 {
    let l = linear.clamp(0.0, 1.0);
    match space {
        // sRGB and Display P3 share the sRGB piecewise transfer.
        WorkingSpace::Srgb | WorkingSpace::DisplayP3 => srgb_oetf(l),
        // Adobe RGB (1998): pure gamma 2.19921875 (= 563/256).
        WorkingSpace::AdobeRgb => l.powf(1.0 / 2.199_218_8),
        // ProPhoto RGB (ROMM): gamma 1.8 with a short linear toe (Et = 1/512).
        WorkingSpace::ProPhoto => {
            const ET: f32 = 1.0 / 512.0;
            if l < ET {
                16.0 * l
            } else {
                l.powf(1.0 / 1.8)
            }
        }
        // BT.2020 / Rec.2020 piecewise OETF.
        WorkingSpace::Rec2020 => {
            const ALPHA: f32 = 1.099_296_8; // 1.09929682680944
            const BETA: f32 = 0.018_053_97; // 0.018053968510807
            if l < BETA {
                4.5 * l
            } else {
                ALPHA * l.powf(0.45) - (ALPHA - 1.0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkingSpace;

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

    #[test]
    fn output_oetf_srgb_and_p3_match_srgb_oetf() {
        for &x in &[0.0_f32, 0.001, 0.05, 0.5, 1.0] {
            assert_eq!(output_oetf(WorkingSpace::Srgb, x), srgb_oetf(x));
            assert_eq!(output_oetf(WorkingSpace::DisplayP3, x), srgb_oetf(x));
        }
    }

    #[test]
    fn output_oetf_endpoints_are_zero_and_one() {
        for &ws in &WorkingSpace::ALL {
            assert!((output_oetf(ws, 0.0)).abs() < 1e-6, "{ws:?} @0");
            assert!((output_oetf(ws, 1.0) - 1.0).abs() < 1e-4, "{ws:?} @1");
        }
    }

    #[test]
    fn output_oetf_clamps_out_of_range() {
        assert_eq!(output_oetf(WorkingSpace::Srgb, -0.5), 0.0);
        assert!((output_oetf(WorkingSpace::Srgb, 2.0) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn output_oetf_adobe_rgb_is_pure_gamma() {
        // Adobe RGB (1998): gamma 2.19921875; encoded = linear^(1/2.19921875).
        let want = 0.5_f32.powf(1.0 / 2.199_218_8);
        assert!((output_oetf(WorkingSpace::AdobeRgb, 0.5) - want).abs() < 1e-4);
    }

    #[test]
    fn output_oetf_is_monotonic() {
        for &ws in &WorkingSpace::ALL {
            let mut prev = -1.0_f32;
            for i in 0..=20 {
                let v = output_oetf(ws, i as f32 / 20.0);
                assert!(v >= prev - 1e-6, "{ws:?} not monotonic at {i}");
                prev = v;
            }
        }
    }
}
