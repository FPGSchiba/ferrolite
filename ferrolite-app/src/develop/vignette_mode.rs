//! Pure, UI-independent helpers for the mode-aware Vignetting control (Spec 4.4,
//! MV2). The Vignetting Amount slider — and the pair of uniforms pushed to the
//! render pipelines — depend on whether a *profile* vignette LUT is currently
//! bound to the viewer:
//!
//! - **Profile mode** (`has_vignette_lut == true`): a matched lens produced a
//!   baked `VignetteMap`. The slider is the profile-correction strength
//!   (`0.0..=2.0`, default `1.0`, unipolar). The stored `Correction.amount`
//!   *is* that strength, and the pipeline lerps the LUT by it
//!   (`set_vig_amount`); the parametric `set_vig_manual` is held at `0.0`.
//! - **Manual mode** (`has_vignette_lut == false`): there is no LUT, so
//!   vignetting is a lens-free parametric gain. The slider is the bipolar
//!   manual strength (`-1.0..=1.0`, default `0.0`; negative darkens corners,
//!   positive brightens). The stored `Correction.amount` *is* that value, and
//!   the pipeline applies it via `set_vig_manual`; the profile lerp
//!   (`set_vig_amount`) is held at `0.0`.
//!
//! Because both meanings live in the same persisted `Correction.amount` field
//! (an `f32`), its interpretation is *mode-derived*: the same number is a
//! `0..2` profile strength or a `-1..1` manual gain depending on
//! `has_vignette_lut`. Persistence is unchanged; only the reading of the value
//! differs by mode.

use ferrolite_pipeline::LensCorrection;

/// Slider parameters (min/max/default/bipolar) for the Vignetting Amount slider
/// in the given mode. Distortion/TCA sliders are always `0..2` (default `1.0`,
/// unipolar) and do not use this.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VigSliderParams {
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub bipolar: bool,
}

/// Profile-mode Amount slider: profile-correction strength.
pub const PROFILE_PARAMS: VigSliderParams = VigSliderParams {
    min: 0.0,
    max: 2.0,
    default: 1.0,
    bipolar: false,
};

/// Manual-mode Amount slider: bipolar lens-free gain.
pub const MANUAL_PARAMS: VigSliderParams = VigSliderParams {
    min: -1.0,
    max: 1.0,
    default: 0.0,
    bipolar: true,
};

/// The slider parameters for the current mode.
pub fn slider_params(has_vignette_lut: bool) -> VigSliderParams {
    if has_vignette_lut {
        PROFILE_PARAMS
    } else {
        MANUAL_PARAMS
    }
}

/// The pair of vignette uniforms `(vig_amount, vig_manual)` to push to a
/// pipeline for the given lens correction and mode. Both `EditPipeline` and
/// `TileEditPipeline` take these two values through identical setters, so every
/// apply site computes the pair here and feeds it to both tiers:
///
/// - profile mode (`has_vignette_lut`): `(vignette_amount(lc), 0.0)` — the LUT
///   lerp is live, the parametric gain is off.
/// - manual mode: `(0.0, manual_from(lc))` — the parametric gain is live, the
///   LUT lerp is off (there is no LUT anyway).
pub fn vig_pair(lc: Option<&LensCorrection>, has_vignette_lut: bool) -> (f32, f32) {
    if has_vignette_lut {
        (ferrolite_pipeline::vignette_amount(lc), 0.0)
    } else {
        (0.0, manual_from(lc))
    }
}

/// The manual (lens-free) vignette gain to push via `set_vig_manual` for a given
/// lens correction: the stored bipolar `vignetting.amount` when vignetting is
/// enabled, otherwise `0.0` (no manual gain). This is only meaningful in manual
/// mode; in profile mode the caller pushes `0.0` for the manual uniform instead.
pub fn manual_from(lc: Option<&LensCorrection>) -> f32 {
    match lc {
        Some(l) if l.vignetting.enabled => l.vignetting.amount,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{Correction, LensCorrection};

    fn lc_with_vig(enabled: bool, amount: f32) -> LensCorrection {
        LensCorrection {
            lens_id: None,
            focal_len: 50.0,
            aperture: 8.0,
            crop_factor: 1.0,
            distortion: Correction::default(),
            tca: Correction::default(),
            vignetting: Correction { enabled, amount },
        }
    }

    #[test]
    fn profile_mode_params_are_unipolar_0_to_2() {
        let p = slider_params(true);
        assert_eq!(p.min, 0.0);
        assert_eq!(p.max, 2.0);
        assert_eq!(p.default, 1.0);
        assert!(!p.bipolar);
    }

    #[test]
    fn manual_mode_params_are_bipolar_minus1_to_1() {
        let p = slider_params(false);
        assert_eq!(p.min, -1.0);
        assert_eq!(p.max, 1.0);
        assert_eq!(p.default, 0.0);
        assert!(p.bipolar);
    }

    #[test]
    fn vig_pair_profile_mode_lerps_lut_and_zeros_manual() {
        let lc = lc_with_vig(true, 1.5);
        let (amount, manual) = vig_pair(Some(&lc), true);
        assert_eq!(amount, 1.5, "profile lerp uses vignette_amount");
        assert_eq!(manual, 0.0, "manual gain off in profile mode");
    }

    #[test]
    fn vig_pair_manual_mode_zeros_lut_and_uses_gain() {
        let lc = lc_with_vig(true, -0.6);
        let (amount, manual) = vig_pair(Some(&lc), false);
        assert_eq!(amount, 0.0, "LUT lerp off in manual mode");
        assert_eq!(manual, -0.6, "manual gain uses stored amount");
    }

    #[test]
    fn vig_pair_manual_mode_disabled_is_neutral() {
        let lc = lc_with_vig(false, -0.6);
        assert_eq!(vig_pair(Some(&lc), false), (0.0, 0.0));
        assert_eq!(vig_pair(None, false), (0.0, 0.0));
    }

    #[test]
    fn manual_from_none_is_zero() {
        assert_eq!(manual_from(None), 0.0);
    }

    #[test]
    fn manual_from_disabled_is_zero() {
        let lc = lc_with_vig(false, -0.5);
        assert_eq!(manual_from(Some(&lc)), 0.0);
    }

    #[test]
    fn manual_from_enabled_returns_stored_amount() {
        let lc = lc_with_vig(true, -0.4);
        assert_eq!(manual_from(Some(&lc)), -0.4);
        let lc2 = lc_with_vig(true, 0.7);
        assert_eq!(manual_from(Some(&lc2)), 0.7);
    }
}
