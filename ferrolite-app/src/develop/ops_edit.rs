//! Pure helpers: map a UI value to a new immutable `OpStack`. A value at its
//! identity default REMOVES the op so `is_identity()`/`has_edits` stay correct.

use ferrolite_pipeline::{
    sharpen_halo, Contrast, Exposure, LensCorrection, Op, OpStack, Sharpen, WhiteBalance,
};

pub fn set_exposure(s: &OpStack, ev: f32) -> OpStack {
    if ev == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::Exposure)
    } else {
        s.set_op(Op::Exposure(Exposure { ev }))
    }
}

pub fn set_white_balance(s: &OpStack, temp: f32, tint: f32) -> OpStack {
    if temp == 0.0 && tint == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::WhiteBalance)
    } else {
        s.set_op(Op::WhiteBalance(WhiteBalance { temp, tint }))
    }
}

pub fn set_contrast(s: &OpStack, amount: f32) -> OpStack {
    if amount == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::Contrast)
    } else {
        s.set_op(Op::Contrast(Contrast { amount }))
    }
}

pub fn set_sharpen(s: &OpStack, amount: f32, radius: u32) -> OpStack {
    if amount == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::Sharpen)
    } else {
        s.set_op(Op::Sharpen(Sharpen { amount, radius }))
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

/// The full-res `TileEditPipeline` bakes geometry + the sharpen/lens halo and
/// the warp grid at construction; only a change to geometry, the sharpen halo,
/// or the rebuild-relevant lens key requires discarding + rebuilding it.
/// Color-only changes (and lens/vignette Amount-only changes) are applied via
/// `TileEditPipeline::set_stack` / the lens-uniform setters without a rebuild.
pub fn needs_full_rebuild(old: &OpStack, new: &OpStack) -> bool {
    old.geometry() != new.geometry()
        || sharpen_halo(old.sharpen()) != sharpen_halo(new.sharpen())
        || lens_rebuild_key(old) != lens_rebuild_key(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{Op, OpStack};

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
    fn set_exposure_adds_then_identity_removes() {
        let s = set_exposure(&OpStack::default(), 0.5);
        assert_eq!(s.exposure().unwrap().ev, 0.5);
        let s2 = set_exposure(&s, 0.0);
        assert!(s2.exposure().is_none(), "identity ev removes the op");
        assert!(s2.is_identity());
    }

    #[test]
    fn set_white_balance_identity_when_both_zero() {
        let s = set_white_balance(&OpStack::default(), 0.0, 0.0);
        assert!(s.white_balance().is_none());
    }

    #[test]
    fn set_sharpen_identity_when_amount_zero() {
        let s = set_sharpen(&OpStack::default(), 0.0, 3);
        assert!(s.sharpen().is_none(), "zero amount = no sharpen");
        let s = set_sharpen(&OpStack::default(), 0.4, 2);
        assert_eq!(
            s.sharpen(),
            Some(ferrolite_pipeline::Sharpen {
                amount: 0.4,
                radius: 2
            })
        );
    }

    #[test]
    fn needs_full_rebuild_on_geometry_and_halo_only() {
        let base = set_exposure(&OpStack::default(), 0.5);
        let color_only = set_contrast(&base, 0.3);
        assert!(
            !needs_full_rebuild(&base, &color_only),
            "color ops: no rebuild"
        );
        let sharper = set_sharpen(&base, 0.5, 5);
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
}
