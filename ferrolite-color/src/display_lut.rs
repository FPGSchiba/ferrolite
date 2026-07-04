//! Monitor-profile 3D-LUT bake: `working→monitor` baked from a parsed ICC via
//! moxcms, indexed through a gamma shaper. GPU-agnostic (data only).

/// Cube edge length of the display LUT (nodes per axis). Mirrored by
/// `ferrolite-vt`'s LUT texture allocation — the two MUST match.
pub const DISPLAY_LUT_SIZE: u32 = 33;

/// Gamma the LUT index grid is encoded with, concentrating nodes in the
/// shadows. Mirrored by `display.wgsl`'s `shaper_encode` — the two MUST match.
pub const DISPLAY_LUT_SHAPER_GAMMA: f32 = 2.2;

/// LUT index (`[0,1]`) → working-linear input fed to the transform.
pub fn shaper_decode(x: f32) -> f32 {
    x.clamp(0.0, 1.0).powf(DISPLAY_LUT_SHAPER_GAMMA)
}

/// Working-linear value → LUT sample coordinate (`[0,1]`). Inverse of `shaper_decode`.
pub fn shaper_encode(x: f32) -> f32 {
    x.clamp(0.0, 1.0).powf(1.0 / DISPLAY_LUT_SHAPER_GAMMA)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shaper_round_trips() {
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            assert!((shaper_encode(shaper_decode(x)) - x).abs() < 1e-5, "x={x}");
        }
    }

    #[test]
    fn shaper_endpoints_are_fixed() {
        assert!((shaper_decode(0.0)).abs() < 1e-6);
        assert!((shaper_decode(1.0) - 1.0).abs() < 1e-6);
    }
}
