//! Pure CPU math turning UI op params into GPU shader uniforms, plus the
//! `#[repr(C)]` Pod uniform structs (layouts MIRROR the WGSL `struct P` in each
//! shader). Display-linear space; the sRGB OETF lives only in the display/blit
//! shader. No GPU here — fully unit-tested.

use crate::op::{Contrast, Exposure, WhiteBalance};

/// Mid-grey pivot (display-linear) about which contrast scales. Placeholder
/// constant; Spec 3 may refine once the working space is fixed.
pub const CONTRAST_PIVOT: f32 = 0.18;

/// EV (stops) -> linear gain. `2^ev`. ev=0 -> 1.0 (identity).
pub fn exposure_gain(ev: f32) -> f32 {
    2.0f32.powf(ev)
}

/// Normalized temp/tint in [-1,1] -> per-channel linear multipliers `[r,g,b]`.
/// Pragmatic placeholder (image science is secondary): warm temp boosts R /
/// cuts B; magenta tint cuts G. Clamped non-negative.
pub fn wb_multipliers(temp: f32, tint: f32) -> [f32; 3] {
    let r = (1.0 + 0.5 * temp).max(0.0);
    let b = (1.0 - 0.5 * temp).max(0.0);
    let g = (1.0 - 0.5 * tint).max(0.0);
    [r, g, b]
}

/// Bipolar amount -> (gain, pivot). amount=0 -> gain 1.0 (identity).
pub fn contrast_gain_pivot(amount: f32) -> (f32, f32) {
    (1.0 + amount, CONTRAST_PIVOT)
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ExposureUniform {
    pub gain: f32,
    pub pad: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WbUniform {
    pub mul: [f32; 3],
    pub pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ContrastUniform {
    pub gain: f32,
    pub pivot: f32,
    pub pad: [f32; 2],
}

pub fn exposure_uniform(op: Option<Exposure>) -> ExposureUniform {
    let ev = op.map(|e| e.ev).unwrap_or(0.0);
    ExposureUniform {
        gain: exposure_gain(ev),
        pad: [0.0; 3],
    }
}

pub fn wb_uniform(op: Option<WhiteBalance>) -> WbUniform {
    let (t, ti) = op.map(|w| (w.temp, w.tint)).unwrap_or((0.0, 0.0));
    WbUniform {
        mul: wb_multipliers(t, ti),
        pad: 0.0,
    }
}

pub fn contrast_uniform(op: Option<Contrast>) -> ContrastUniform {
    let a = op.map(|c| c.amount).unwrap_or(0.0);
    let (gain, pivot) = contrast_gain_pivot(a);
    ContrastUniform {
        gain,
        pivot,
        pad: [0.0; 2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_gain_is_two_to_the_ev() {
        assert!((exposure_gain(0.0) - 1.0).abs() < 1e-6);
        assert!((exposure_gain(1.0) - 2.0).abs() < 1e-6);
        assert!((exposure_gain(-1.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn wb_identity_at_zero() {
        assert_eq!(wb_multipliers(0.0, 0.0), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn wb_warm_temp_boosts_red_cuts_blue() {
        assert_eq!(wb_multipliers(1.0, 0.0), [1.5, 1.0, 0.5]);
    }

    #[test]
    fn wb_magenta_tint_cuts_green() {
        assert_eq!(wb_multipliers(0.0, 1.0), [1.0, 0.5, 1.0]);
    }

    #[test]
    fn contrast_identity_and_gain() {
        assert_eq!(contrast_gain_pivot(0.0), (1.0, CONTRAST_PIVOT));
        assert_eq!(contrast_gain_pivot(1.0), (2.0, CONTRAST_PIVOT));
    }

    #[test]
    fn uniform_constructors_use_identity_when_absent() {
        assert_eq!(exposure_uniform(None).gain, 1.0);
        assert_eq!(wb_uniform(None).mul, [1.0, 1.0, 1.0]);
        assert_eq!(contrast_uniform(None).gain, 1.0);
    }
}
