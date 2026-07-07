//! Correlated-colour-temperature ⇄ CIE 1931 xy helpers.
//!
//! `cct_to_xy` follows Kim et al.'s (2002) cubic Planckian-locus approximation
//! (valid 1667–25000 K); `xy_to_cct` uses McCamy's cubic approximation. Together
//! they round-trip to ~0.2 % across the 2000–7000 K range covering the DNG
//! calibration illuminants (Standard-A ≈ 2856 K, D65 ≈ 6504 K), matching DNG's
//! interpolation domain closely enough (P2 spec §8). Pure, `unsafe`-free.

use crate::matrix::Xy;

/// Correlated colour temperature (Kelvin) → CIE 1931 xy on the Planckian locus.
/// Kim et al. (2002); input clamped to the approximation's valid 1667–25000 K.
pub fn cct_to_xy(cct_k: f32) -> Xy {
    let t = f64::from(cct_k.clamp(1667.0, 25000.0));
    let inv = 1.0 / t;
    let inv2 = inv * inv;
    let inv3 = inv2 * inv;
    let x = if t <= 4000.0 {
        -0.266_123_9e9 * inv3 - 0.234_358_9e6 * inv2 + 0.877_695_6e3 * inv + 0.179_910
    } else {
        -3.025_846_9e9 * inv3 + 2.107_037_9e6 * inv2 + 0.222_634_7e3 * inv + 0.240_390
    };
    let x2 = x * x;
    let x3 = x2 * x;
    let y = if t <= 2222.0 {
        -1.106_381_4 * x3 - 1.348_110_2 * x2 + 2.185_558_32 * x - 0.202_196_83
    } else if t <= 4000.0 {
        -0.954_947_6 * x3 - 1.374_185_93 * x2 + 2.091_370_15 * x - 0.167_488_67
    } else {
        3.081_758_0 * x3 - 5.873_386_7 * x2 + 3.751_129_97 * x - 0.370_014_83
    };
    Xy {
        x: x as f32,
        y: y as f32,
    }
}

/// CIE 1931 xy → correlated colour temperature (Kelvin), McCamy's approximation.
pub fn xy_to_cct(xy: Xy) -> f32 {
    let n = (xy.x - 0.3320) / (0.1858 - xy.y);
    449.0 * n * n * n + 3525.0 * n * n + 6823.3 * n + 5520.33
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cct_to_xy_near_d65() {
        // D65 ≈ 6504 K sits just off the Planckian locus; expect close xy.
        let xy = cct_to_xy(6504.0);
        assert!((xy.x - 0.3128).abs() < 0.01, "x={}", xy.x);
        assert!((xy.y - 0.3290).abs() < 0.02, "y={}", xy.y);
    }

    #[test]
    fn cct_to_xy_near_standard_a() {
        // Standard illuminant A = 2856 K, xy ≈ (0.4476, 0.4074).
        let xy = cct_to_xy(2856.0);
        assert!((xy.x - 0.4476).abs() < 0.01, "x={}", xy.x);
        assert!((xy.y - 0.4074).abs() < 0.01, "y={}", xy.y);
    }

    #[test]
    fn xy_to_cct_recovers_standard_a() {
        let cct = xy_to_cct(Xy { x: 0.4476, y: 0.4074 });
        assert!((cct - 2856.0).abs() < 100.0, "cct={cct}");
    }

    #[test]
    fn round_trips_within_two_percent() {
        for &t in &[2856.0_f32, 3500.0, 5000.0, 6504.0] {
            let back = xy_to_cct(cct_to_xy(t));
            let rel = (back - t).abs() / t;
            assert!(rel < 0.02, "T={t} round-tripped to {back} (rel {rel})");
        }
    }

    #[test]
    fn cct_to_xy_clamps_out_of_range_input() {
        // Below/above the approximation's valid range must not produce NaN/Inf.
        for &t in &[100.0_f32, 1e6] {
            let xy = cct_to_xy(t);
            assert!(xy.x.is_finite() && xy.y.is_finite(), "T={t} -> {xy:?}");
        }
    }
}
