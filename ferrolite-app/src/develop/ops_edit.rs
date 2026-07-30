//! Pure helpers: map a UI value to a new immutable `OpStack`. A value at its
//! identity default REMOVES the op so `is_identity()`/`has_edits` stay correct.
//!
//! Tone curve and color grade no longer route through dedicated `set_*`
//! helpers here (Phase 2b Task 3): both are now `AdjustmentSet` fields written
//! via the scoped-edit path (`crate::develop::scope::ScopedEdit::write`),
//! whose `with_global`/`with_layer_adjustments` normalize identity structures
//! away doc-side, same effect as the old identity-eliding helpers.

use ferrolite_pipeline::{sharpen_halo_doc, LensCorrection, Op, OpStack};

/// The stack whose render the viewer should SHOW this frame — the single
/// source of truth for what extent the preview tier evaluates to:
///
/// * before/after: the identity stack (the "before" image);
/// * crop mode (`crop_active`): the live stack with the crop FORCED FULL —
///   rotation/aspect/keystone stay applied so the Angle slider rotates live,
///   but the crop rectangle is represented by the overlay drawn over the
///   full image, so the render must be the FULL (uncropped) extent;
/// * otherwise: the live stack unchanged (crop applied → cropped extent).
///
/// Entering/leaving crop mode therefore CHANGES the shown extent (full ↔
/// cropped), which is why the crop-mode transition must also re-frame the
/// view to the newly shown dims (see the `SetPreviewAndFull` handler in
/// `app.rs`) — leaving the old fit made re-editing a crop open visibly
/// more zoomed-in than the tool was left (the fit belonged to the smaller
/// cropped extent) and desynced the overlay's `image_dims`-derived hit
/// geometry from what was actually displayed.
pub fn shown_stack(stack: &OpStack, before_after: bool, crop_active: bool) -> OpStack {
    let mut shown = if before_after {
        OpStack::default()
    } else {
        stack.clone()
    };
    if crop_active {
        if let Some(g) = shown.geometry() {
            shown = shown.set_op(Op::Geometry(ferrolite_pipeline::Geometry {
                crop: ferrolite_pipeline::CropRect::full(),
                angle_deg: g.angle_deg,
                aspect: g.aspect,
                keystone_v: g.keystone_v,
                keystone_h: g.keystone_h,
            }));
        }
    }
    shown
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
/// The sharpen halo uses `sharpen_halo_doc` (Phase 4 Task 4): the max radius
/// over the global `Sharpen` op AND every visible mask layer's own active
/// sharpen — a per-mask sharpen is a real per-pixel neighbourhood op with its
/// own radius, so a layer-only sharpen change (global op absent/unchanged)
/// must still force a rebuild when it changes the document-wide max.
///
/// Dehaze does NOT force a rebuild (ST-Task 3): `dehaze_halo` is now always 0
/// (the tiled dehaze recovery samples a shared whole-image transmission, no
/// per-tile neighbourhood), so an amount/radius change is a `set_stack`
/// uniform update + a re-wired shared transmission (see `EditTileProducer`),
/// same as any other color op — never a `TileEditPipeline` rebuild.
pub fn needs_full_rebuild(old: &OpStack, new: &OpStack) -> bool {
    old.geometry() != new.geometry()
        || sharpen_halo_doc(old) != sharpen_halo_doc(new)
        || lens_rebuild_key(old) != lens_rebuild_key(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{Dehaze, Op, OpStack, Sharpen};

    fn cropped_rotated_stack() -> OpStack {
        OpStack::default().set_op(Op::Geometry(ferrolite_pipeline::Geometry {
            crop: ferrolite_pipeline::CropRect {
                x: 0.1,
                y: 0.1,
                w: 0.5,
                h: 0.5,
            },
            angle_deg: 7.5,
            ..Default::default()
        }))
    }

    /// The refit-dims choice (crop re-edit bug): entering crop mode shows the
    /// FULL extent — the shown stack's crop must be forced full while rotation
    /// (and the rest of the geometry) stays applied.
    #[test]
    fn shown_stack_in_crop_mode_forces_crop_full_but_keeps_rotation() {
        let stack = cropped_rotated_stack();
        let shown = shown_stack(&stack, false, true);
        let g = shown.geometry().expect("geometry kept");
        assert_eq!(
            (g.crop.x, g.crop.y, g.crop.w, g.crop.h),
            (0.0, 0.0, 1.0, 1.0),
            "crop forced full while the tool is active"
        );
        assert_eq!(g.angle_deg, 7.5, "rotation stays live in crop mode");
        // The LIVE stack is untouched (immutability) — only the shown copy changes.
        assert_eq!(stack.geometry().unwrap().crop.w, 0.5);
    }

    /// Leaving crop mode shows the CROPPED extent again: the shown stack is
    /// the live stack unchanged.
    #[test]
    fn shown_stack_outside_crop_mode_keeps_the_crop() {
        let stack = cropped_rotated_stack();
        let shown = shown_stack(&stack, false, false);
        assert_eq!(
            shown.geometry().unwrap().crop,
            stack.geometry().unwrap().crop,
            "at rest the cropped extent is shown"
        );
    }

    /// Before/after shows the identity render; crop mode is then a no-op on
    /// it (no geometry op to force full).
    #[test]
    fn shown_stack_before_after_is_identity_even_in_crop_mode() {
        let stack = cropped_rotated_stack();
        let shown = shown_stack(&stack, true, true);
        assert!(shown.is_identity());
        assert!(shown.geometry().is_none());
    }

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
            ..Default::default()
        }));
        assert!(needs_full_rebuild(&base, &geo), "geometry change: rebuild");
    }

    /// Plan `crop-overhaul` C4 Task 4: keystone is a geometry-tier change,
    /// same treatment as `angle_deg` — a keystone-only edit (crop/angle/aspect
    /// unchanged) must still force the full-res `TileEditPipeline` rebuild.
    #[test]
    fn needs_full_rebuild_on_keystone_only_change() {
        let base = OpStack::default().set_op(Op::Geometry(ferrolite_pipeline::Geometry {
            crop: ferrolite_pipeline::CropRect::full(),
            angle_deg: 0.0,
            aspect: ferrolite_pipeline::Aspect::Original,
            keystone_v: 0.0,
            keystone_h: 0.0,
        }));
        let keystone_v_only = base.set_op(Op::Geometry(ferrolite_pipeline::Geometry {
            keystone_v: 0.3,
            ..base.geometry().unwrap()
        }));
        assert!(
            needs_full_rebuild(&base, &keystone_v_only),
            "keystone_v-only change: rebuild"
        );
        let keystone_h_only = base.set_op(Op::Geometry(ferrolite_pipeline::Geometry {
            keystone_h: -0.4,
            ..base.geometry().unwrap()
        }));
        assert!(
            needs_full_rebuild(&base, &keystone_h_only),
            "keystone_h-only change: rebuild"
        );
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

    /// Phase 4 Task 4: a per-mask sharpen radius is a REAL per-pixel
    /// neighbourhood op (its own separable blur), so a change to a visible
    /// mask layer's sharpen — even with the GLOBAL sharpen op absent/
    /// unchanged — must force a rebuild when it changes the document-wide
    /// max halo (`sharpen_halo_doc`).
    #[test]
    fn mask_sharpen_forces_rebuild_via_halo() {
        use ferrolite_pipeline::{AdjustmentSet, LocalAdjustments, MaskLayer, Sharpen};

        let mask_layer = |amount: f32, radius: u32| LocalAdjustments {
            layers: vec![MaskLayer {
                name: "l".into(),
                visible: true,
                mask: Default::default(),
                adjustments: AdjustmentSet {
                    sharpen: Sharpen { amount, radius },
                    ..Default::default()
                },
            }],
        };

        let base = OpStack::default();
        let layer_sharpen = base.set_op(Op::LocalAdjustments(mask_layer(0.5, 5)));
        assert!(
            needs_full_rebuild(&base, &layer_sharpen),
            "mask-only sharpen (global absent): halo change forces rebuild"
        );

        // A larger mask radius (global still absent) also forces a rebuild.
        let layer_sharpen_bigger = base.set_op(Op::LocalAdjustments(mask_layer(0.5, 9)));
        assert!(
            needs_full_rebuild(&layer_sharpen, &layer_sharpen_bigger),
            "mask sharpen radius growth forces rebuild"
        );

        // Amount-only change on the mask layer, radius unchanged: halo is
        // unaffected (amount doesn't change the radius), so no rebuild.
        let layer_sharpen_amt = base.set_op(Op::LocalAdjustments(mask_layer(0.9, 5)));
        assert!(
            !needs_full_rebuild(&layer_sharpen, &layer_sharpen_amt),
            "mask sharpen amount-only: no halo change, no rebuild"
        );

        // A hidden mask layer's sharpen never contributes to the halo.
        let mut hidden_la = mask_layer(0.5, 9);
        hidden_la.layers[0].visible = false;
        let hidden = base.set_op(Op::LocalAdjustments(hidden_la));
        assert!(
            !needs_full_rebuild(&base, &hidden),
            "hidden mask layer's sharpen: no halo contribution, no rebuild"
        );
    }
}
