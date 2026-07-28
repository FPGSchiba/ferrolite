//! Pure helpers: map a UI value to a new immutable `OpStack`. A value at its
//! identity default REMOVES the op so `is_identity()`/`has_edits` stay correct.

use ferrolite_pipeline::{sharpen_halo, ColorGrade, LensCorrection, Op, OpStack, ToneCurve};

/// Set the tone curve, or REMOVE the op entirely when the whole curve (Master +
/// R/G/B + parametric) is identity — so `is_identity()`/`has_edits` stay correct,
/// mirroring every other `set_*` helper here.
pub fn set_tone_curve(s: &OpStack, tc: ToneCurve) -> OpStack {
    if tc.is_identity() {
        s.reset(ferrolite_pipeline::OpKind::ToneCurve)
    } else {
        s.set_op(Op::ToneCurve(tc))
    }
}

/// Set the color grade, or REMOVE the op entirely when every wheel is neutral
/// (no tint, no lum) — so `is_identity()`/`has_edits` stay correct, mirroring
/// every other `set_*` helper here.
pub fn set_color_grade(s: &OpStack, cg: ColorGrade) -> OpStack {
    if cg.is_identity() {
        s.reset(ferrolite_pipeline::OpKind::ColorGrade)
    } else {
        s.set_op(Op::ColorGrade(cg))
    }
}

/// A `LensCorrection` with no matched lens AND every correction disabled is
/// identity (nothing to bake, nothing to apply) → remove the op entirely so
/// `is_identity()`/`has_edits` stay correct, mirroring every other `set_*`
/// helper in this file.
pub fn set_lens_correction(s: &OpStack, lc: LensCorrection) -> OpStack {
    let identity =
        lc.lens_id.is_none() && !lc.distortion.enabled && !lc.tca.enabled && !lc.vignetting.enabled;
    if identity {
        s.reset(ferrolite_pipeline::OpKind::LensCorrection)
    } else {
        s.set_op(Op::LensCorrection(lc))
    }
}

/// The rebuild-relevant lens fingerprint: everything that changes the baked
/// warp grid (hence the halo). Deliberately EXCLUDES the per-correction
/// `amount`s — those are uniform-only updates (`set_lens_uniform`), not a
/// rebuild. `lens_id`/`focal_len`/`aperture`/`crop_factor` all feed the bake, so
/// a change to any of them yields a new grid + halo and must rebuild.
pub(crate) fn lens_rebuild_key(s: &OpStack) -> (Option<String>, bool, bool, u32, u32, u32) {
    match s.lens_correction() {
        Some(l) => (
            l.lens_id,
            l.distortion.enabled,
            l.tca.enabled,
            l.focal_len.to_bits(),
            l.aperture.to_bits(),
            l.crop_factor.to_bits(),
        ),
        None => (None, false, false, 0, 0, 0),
    }
}

/// The bake-trigger lens fingerprint: everything that changes the baked
/// PRODUCTS (`bake_products`'s warp grid AND vignette LUT) — i.e.
/// `lens_rebuild_key` PLUS `vignetting.enabled`. This is deliberately a
/// SEPARATE key from `lens_rebuild_key`: `bake_products` bakes the
/// `VignetteMap` whenever `vignetting.enabled`, so a vignetting toggle must
/// spawn a bake even though it has no halo/geometry impact and therefore must
/// NOT force an immediate `TileEditPipeline` rebuild (that happens once,
/// naturally, when the bake result lands in `apply_lens_baked` and rebinds
/// the producer). Use this key to decide whether to spawn a bake; use
/// `lens_rebuild_key` to decide whether to rebuild the full-res pipeline.
pub fn lens_bake_key(s: &OpStack) -> (Option<String>, bool, bool, bool, u32, u32, u32) {
    match s.lens_correction() {
        Some(l) => (
            l.lens_id,
            l.distortion.enabled,
            l.tca.enabled,
            l.vignetting.enabled,
            l.focal_len.to_bits(),
            l.aperture.to_bits(),
            l.crop_factor.to_bits(),
        ),
        None => (None, false, false, false, 0, 0, 0),
    }
}

/// The full-res `TileEditPipeline` bakes geometry + the sharpen/lens halo and
/// the warp grid at construction; only a change to geometry, the sharpen halo,
/// or the rebuild-relevant lens key requires discarding + rebuilding it.
/// Color-only changes (and lens/vignette Amount-only changes) are applied via
/// `TileEditPipeline::set_stack` / the lens-uniform setters without a rebuild.
///
/// Dehaze does NOT force a rebuild (ST-Task 3): `dehaze_halo` is now always 0
/// (the tiled dehaze recovery samples a shared whole-image transmission, no
/// per-tile neighbourhood), so an amount/radius change is a `set_stack`
/// uniform update + a re-wired shared transmission (see `EditTileProducer`),
/// same as any other color op — never a `TileEditPipeline` rebuild.
pub fn needs_full_rebuild(old: &OpStack, new: &OpStack) -> bool {
    old.geometry() != new.geometry()
        || sharpen_halo(old.sharpen()) != sharpen_halo(new.sharpen())
        || lens_rebuild_key(old) != lens_rebuild_key(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{Dehaze, Op, OpStack, Sharpen};

    #[test]
    fn set_lens_correction_removes_when_unmatched_and_all_off() {
        use ferrolite_pipeline::{Correction, LensCorrection};
        let off = LensCorrection {
            lens_id: None,
            focal_len: 24.0,
            aperture: 8.0,
            crop_factor: 1.0,
            distortion: Correction::default(),
            tca: Correction::default(),
            vignetting: Correction::default(),
        };
        let s = set_lens_correction(&OpStack::default(), off.clone());
        assert!(
            s.lens_correction().is_none(),
            "no lens + all off = identity"
        );

        let on = LensCorrection {
            lens_id: Some("EF 24-70".into()),
            distortion: Correction {
                enabled: true,
                amount: 1.0,
            },
            ..off
        };
        let s2 = set_lens_correction(&OpStack::default(), on);
        assert!(s2.lens_correction().is_some());
    }

    #[test]
    fn lens_bake_key_includes_vignetting_but_rebuild_key_does_not() {
        use ferrolite_pipeline::{Correction, LensCorrection};
        let base_lc = LensCorrection {
            lens_id: Some("EF 24-70".into()),
            focal_len: 24.0,
            aperture: 8.0,
            crop_factor: 1.0,
            distortion: Correction {
                enabled: true,
                amount: 1.0,
            },
            tca: Correction::default(),
            vignetting: Correction {
                enabled: false,
                amount: 1.0,
            },
        };
        let base = set_lens_correction(&OpStack::default(), base_lc.clone());

        // Toggling vignetting.enabled: lens_bake_key changes, lens_rebuild_key doesn't.
        let vig_on_lc = LensCorrection {
            vignetting: Correction {
                enabled: true,
                amount: 1.0,
            },
            ..base_lc.clone()
        };
        let vig_on = set_lens_correction(&base, vig_on_lc);
        assert_ne!(
            lens_bake_key(&base),
            lens_bake_key(&vig_on),
            "vignetting toggle must change the bake key so a bake fires"
        );
        assert_eq!(
            lens_rebuild_key(&base),
            lens_rebuild_key(&vig_on),
            "vignetting toggle must NOT change the halo-rebuild key"
        );

        // Amount-only change on vignetting: changes NEITHER key.
        let amount_only_lc = LensCorrection {
            vignetting: Correction {
                enabled: true,
                amount: 1.5,
            },
            ..vig_on.lens_correction().unwrap()
        };
        let amount_only = set_lens_correction(&vig_on, amount_only_lc);
        assert_eq!(
            lens_bake_key(&vig_on),
            lens_bake_key(&amount_only),
            "Amount-only change must not change the bake key"
        );
        assert_eq!(
            lens_rebuild_key(&vig_on),
            lens_rebuild_key(&amount_only),
            "Amount-only change must not change the rebuild key"
        );
    }

    #[test]
    fn needs_full_rebuild_on_geometry_and_halo_only() {
        use ferrolite_pipeline::{Contrast, Exposure};
        let base = OpStack::default().set_op(Op::Exposure(Exposure { ev: 0.5 }));
        let color_only = base.set_op(Op::Contrast(Contrast { amount: 0.3 }));
        assert!(
            !needs_full_rebuild(&base, &color_only),
            "color ops: no rebuild"
        );
        let sharper = base.set_op(Op::Sharpen(Sharpen {
            amount: 0.5,
            radius: 5,
        }));
        assert!(needs_full_rebuild(&base, &sharper), "halo change: rebuild");
        let geo = base.set_op(Op::Geometry(ferrolite_pipeline::Geometry {
            crop: ferrolite_pipeline::CropRect::full(),
            angle_deg: 5.0,
            aspect: ferrolite_pipeline::Aspect::Free,
        }));
        assert!(needs_full_rebuild(&base, &geo), "geometry change: rebuild");
    }

    #[test]
    fn needs_full_rebuild_on_lens_enable_and_lens_change() {
        let base = OpStack::default();
        let lc = |dist_on: bool, id: &str| ferrolite_pipeline::LensCorrection {
            lens_id: Some(id.into()),
            focal_len: 24.0,
            aperture: 8.0,
            crop_factor: 1.0,
            distortion: ferrolite_pipeline::Correction {
                enabled: dist_on,
                amount: 1.0,
            },
            tca: ferrolite_pipeline::Correction::default(),
            vignetting: ferrolite_pipeline::Correction::default(),
        };
        let on = base.set_op(Op::LensCorrection(lc(true, "A")));
        assert!(
            needs_full_rebuild(&base, &on),
            "enabling distortion changes the halo"
        );
        // Amount-only change must NOT rebuild:
        let mut lc2 = on.lens_correction().unwrap();
        lc2.distortion.amount = 0.5;
        let amt = on.set_op(Op::LensCorrection(lc2));
        assert!(!needs_full_rebuild(&on, &amt), "Amount is uniform-only");
        // Different lens id → rebuild (new grid + halo):
        let other = base.set_op(Op::LensCorrection(lc(true, "B")));
        assert!(needs_full_rebuild(&on, &other));
    }

    #[test]
    fn set_tone_curve_identity_removes_the_op() {
        use ferrolite_pipeline::ToneCurve;
        let s = set_tone_curve(&OpStack::default(), ToneCurve::default());
        assert!(s.tone_curve().is_none(), "fully-identity curve = no op");
        assert!(s.is_identity());
    }

    #[test]
    fn set_tone_curve_master_edit_sets_the_op() {
        use ferrolite_pipeline::{CurveMode, ToneCurve};
        let tc = ToneCurve {
            points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
            mode: CurveMode::Smooth,
            ..Default::default()
        };
        let s = set_tone_curve(&OpStack::default(), tc.clone());
        assert_eq!(s.tone_curve(), Some(tc));
    }

    #[test]
    fn set_tone_curve_channel_only_edit_is_kept() {
        use ferrolite_pipeline::{CurveMode, PointCurve, ToneCurve};
        let tc = ToneCurve {
            blue: PointCurve {
                points: vec![(0.0, 0.0), (0.5, 0.7), (1.0, 1.0)],
                mode: CurveMode::Linear,
            },
            ..Default::default()
        };
        let s = set_tone_curve(&OpStack::default(), tc);
        assert!(
            s.tone_curve().is_some(),
            "a blue-only curve is not identity"
        );
    }

    #[test]
    fn set_tone_curve_parametric_only_edit_is_kept() {
        use ferrolite_pipeline::{ParametricCurve, ToneCurve};
        let tc = ToneCurve {
            parametric: ParametricCurve {
                highlights: -0.5,
                ..Default::default()
            },
            ..Default::default()
        };
        let s = set_tone_curve(&OpStack::default(), tc);
        assert!(s.tone_curve().is_some());
    }

    #[test]
    fn set_tone_curve_split_only_edit_is_kept() {
        // Regression: a parametric SPLIT moved off default (zero regions) must
        // keep the op so the split slider persists instead of snapping back.
        use ferrolite_pipeline::{ParametricCurve, ToneCurve};
        let tc = ToneCurve {
            parametric: ParametricCurve {
                midtone_split: 0.65,
                ..Default::default()
            },
            ..Default::default()
        };
        let s = set_tone_curve(&OpStack::default(), tc);
        assert!(
            s.tone_curve().is_some(),
            "a split-only parametric edit must not be elided"
        );
    }

    #[test]
    fn set_color_grade_blending_or_balance_only_edit_is_kept() {
        // Regression: Blending/Balance moved off default (neutral wheels) must
        // keep the op so those sliders persist instead of snapping back.
        use ferrolite_pipeline::ColorGrade;
        let s = set_color_grade(
            &OpStack::default(),
            ColorGrade {
                blending: 0.8,
                ..Default::default()
            },
        );
        assert!(
            s.color_grade().is_some(),
            "a blending-only grade edit must not be elided"
        );
        let s2 = set_color_grade(
            &OpStack::default(),
            ColorGrade {
                balance: -0.4,
                ..Default::default()
            },
        );
        assert!(
            s2.color_grade().is_some(),
            "a balance-only grade edit must not be elided"
        );
    }

    #[test]
    fn set_color_grade_identity_removes_the_op() {
        use ferrolite_pipeline::ColorGrade;
        let s = set_color_grade(&OpStack::default(), ColorGrade::default());
        assert!(s.color_grade().is_none(), "neutral grade = no op");
        assert!(s.is_identity());
    }

    #[test]
    fn set_color_grade_tinted_wheel_sets_the_op() {
        use ferrolite_pipeline::{ColorGrade, GradeWheel};
        let cg = ColorGrade {
            highlights: GradeWheel {
                hue: 40.0,
                sat: 0.3,
                lum: 0.0,
            },
            ..Default::default()
        };
        let s = set_color_grade(&OpStack::default(), cg);
        assert_eq!(s.color_grade(), Some(cg));
    }

    #[test]
    fn set_color_grade_lum_only_is_kept() {
        use ferrolite_pipeline::{ColorGrade, GradeWheel};
        let cg = ColorGrade {
            global: GradeWheel {
                hue: 0.0,
                sat: 0.0,
                lum: 0.25,
            },
            ..Default::default()
        };
        let s = set_color_grade(&OpStack::default(), cg);
        assert!(
            s.color_grade().is_some(),
            "a lum-only grade is not identity"
        );
    }

    #[test]
    fn dehaze_changes_never_force_a_rebuild() {
        // ST-Task 3: `dehaze_halo` is always 0 — the tiled dehaze recovery
        // samples a shared whole-image transmission (no per-tile
        // neighbourhood), so enabling/disabling dehaze, an amount-only change,
        // and a radius change are all `set_stack`-only, same as a color op.
        let base = OpStack::default();
        let dehaze_op =
            |amount: f32, radius: u32| base.set_op(Op::Dehaze(Dehaze { amount, radius }));
        let on = dehaze_op(0.5, 8);
        assert!(!needs_full_rebuild(&base, &on), "dehaze on: no rebuild");
        let on2 = dehaze_op(0.9, 8);
        assert!(!needs_full_rebuild(&on, &on2), "amount-only: no rebuild");
        let on3 = dehaze_op(0.9, 16);
        assert!(!needs_full_rebuild(&on2, &on3), "radius change: no rebuild");
        assert!(!needs_full_rebuild(&on, &base), "dehaze off: no rebuild");
    }
}
