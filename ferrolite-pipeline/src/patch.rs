//! `EditPatch` — a partial `EditDoc` plus the set of groups it authoritatively
//! writes (P7 design §3). The single currency of presets, copy/paste and sync:
//! all three build one of these and call `apply_to`.
//!
//! Hand-rolled bitflags rather than the `bitflags` crate — P7 adds no
//! dependencies (design §1.7).

use serde::{Deserialize, Serialize};

use crate::op::EditDoc;

/// Preset/patch schema version. Bump only on a breaking layout change.
pub const PATCH_VERSION: u32 = 1;

/// Which adjustment groups a patch writes. Groups outside the set are ignored
/// on read and left untouched on the target.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GroupSet(u16);

impl GroupSet {
    pub const EMPTY: GroupSet = GroupSet(0);
    pub const LIGHT: GroupSet = GroupSet(1 << 0);
    pub const COLOR: GroupSet = GroupSet(1 << 1);
    pub const CURVE: GroupSet = GroupSet(1 << 2);
    pub const HSL: GroupSet = GroupSet(1 << 3);
    pub const GRADING: GroupSet = GroupSet(1 << 4);
    pub const DETAIL: GroupSet = GroupSet(1 << 5);
    pub const EFFECTS: GroupSet = GroupSet(1 << 6);
    pub const GEOMETRY: GroupSet = GroupSet(1 << 7);
    pub const LENS: GroupSet = GroupSet(1 << 8);
    /// Present so a future phase can enable it; `apply_to` always ignores it
    /// (P7 design §2 P7-D2 — masks are out of scope).
    pub const MASKS: GroupSet = GroupSet(1 << 9);

    /// Every group `apply_to` actually honors, in UI order. Excludes MASKS.
    pub const ALL_APPLICABLE: [GroupSet; 9] = [
        GroupSet::LIGHT,
        GroupSet::COLOR,
        GroupSet::CURVE,
        GroupSet::HSL,
        GroupSet::GRADING,
        GroupSet::DETAIL,
        GroupSet::EFFECTS,
        GroupSet::GEOMETRY,
        GroupSet::LENS,
    ];

    pub fn contains(self, other: GroupSet) -> bool {
        self.0 & other.0 == other.0 && other.0 != 0
    }
    pub fn insert(&mut self, other: GroupSet) {
        self.0 |= other.0;
    }
    pub fn remove(&mut self, other: GroupSet) {
        self.0 &= !other.0;
    }
    pub fn union(self, other: GroupSet) -> GroupSet {
        GroupSet(self.0 | other.0)
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub fn bits(self) -> u16 {
        self.0
    }
    pub fn from_bits(bits: u16) -> GroupSet {
        GroupSet(bits)
    }
}

/// A partial edit document: values plus the groups it authoritatively writes.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct EditPatch {
    pub version: u32,
    pub owns: GroupSet,
    /// Value carrier. Only fields in an owned group are meaningful; the rest
    /// hold whatever `Default` produced.
    pub doc: EditDoc,
}

impl EditPatch {
    /// Capture `owns`'s groups from `doc`.
    pub fn from_doc(doc: &EditDoc, owns: GroupSet) -> Self {
        Self {
            version: PATCH_VERSION,
            owns,
            doc: doc.clone(),
        }
    }

    /// Return `target` with every owned group replaced by this patch's values.
    ///
    /// `GroupSet::MASKS` is deliberately NOT handled: mask layers are out of
    /// P7 (design §2 P7-D2), so `layers` always survives from the target even
    /// if a future preset file sets the flag. That is the safe direction — a
    /// patch can never silently destroy mask work.
    pub fn apply_to(&self, target: &EditDoc) -> EditDoc {
        let mut out = target.clone();
        let s = &self.doc;

        if self.owns.contains(GroupSet::LIGHT) {
            out.global.exposure = s.global.exposure;
            out.global.contrast = s.global.contrast;
            out.global.highlights = s.global.highlights;
            out.global.shadows = s.global.shadows;
            out.global.whites = s.global.whites;
            out.global.blacks = s.global.blacks;
        }
        if self.owns.contains(GroupSet::COLOR) {
            out.global.temp = s.global.temp;
            out.global.tint = s.global.tint;
            out.global.saturation = s.global.saturation;
            out.global.hue = s.global.hue;
            out.global.vibrance = s.global.vibrance;
            out.global.color = s.global.color;
        }
        if self.owns.contains(GroupSet::CURVE) {
            out.global.tone_curve = s.global.tone_curve.clone();
        }
        if self.owns.contains(GroupSet::HSL) {
            out.global.hsl = s.global.hsl;
        }
        if self.owns.contains(GroupSet::GRADING) {
            out.global.color_grade = s.global.color_grade;
        }
        if self.owns.contains(GroupSet::DETAIL) {
            out.global.sharpen = s.global.sharpen;
            out.global.noise_reduction = s.global.noise_reduction;
        }
        if self.owns.contains(GroupSet::EFFECTS) {
            out.global.dehaze = s.global.dehaze;
        }
        if self.owns.contains(GroupSet::GEOMETRY) {
            out.geometry = s.geometry;
        }
        if self.owns.contains(GroupSet::LENS) {
            apply_lens_amounts(&mut out, s);
        }
        out
    }
}

/// Copy ONLY the three correction amounts, never the capture context.
///
/// `LensCorrection` carries `lens_id`, `focal_len`, `aperture` and
/// `crop_factor` — all per-image EXIF. Copying those would stamp the source's
/// focal length onto the target and bake a wrong correction (design §3.2,
/// load-bearing). If either side has no `LensCorrection`, this is a no-op: an
/// unmatched target has no context to attach amounts to.
fn apply_lens_amounts(out: &mut EditDoc, source: &EditDoc) {
    let (Some(src), Some(dst)) = (source.lens.as_ref(), out.lens.as_mut()) else {
        return;
    };
    dst.distortion = src.distortion;
    dst.tca = src.tca;
    dst.vignetting = src.vignetting;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::AdjustmentSet;
    use crate::op::{EditDoc, Geometry};

    /// A patch owning only LIGHT writes the light fields and leaves every other
    /// field of the target byte-identical.
    #[test]
    fn owned_group_overwrites_unowned_group_is_untouched() {
        let mut source = EditDoc::default();
        source.global.exposure = 1.5;
        source.global.saturation = 0.9; // COLOR — not owned, must not travel

        let mut target = EditDoc::default();
        target.global.exposure = -0.25;
        target.global.saturation = 0.1;
        target.geometry = Some(Geometry {
            crop: crate::op::CropRect {
                x: 0.1,
                y: 0.1,
                w: 0.8,
                h: 0.8,
            },
            ..Default::default()
        });

        let patch = EditPatch::from_doc(&source, GroupSet::LIGHT);
        let out = patch.apply_to(&target);

        assert_eq!(out.global.exposure, 1.5, "owned LIGHT must overwrite");
        assert_eq!(out.global.saturation, 0.1, "unowned COLOR must not travel");
        assert_eq!(out.geometry, target.geometry, "unowned GEOMETRY untouched");
    }

    /// LENS must carry the three correction AMOUNTS and never the capture
    /// context. Copying `focal_len`/`lens_id` would bake the source lens's
    /// correction into a photo shot on a different lens.
    #[test]
    // default-then-assign mirrors the plan's literal test spec; clearer than
    // struct-update for single fields.
    #[allow(clippy::field_reassign_with_default)]
    fn lens_group_copies_amounts_but_never_capture_context() {
        use crate::op::{Correction, LensCorrection};
        let lens = |id: &str, focal: f32, amount: f32| LensCorrection {
            lens_id: Some(id.to_string()),
            focal_len: focal,
            aperture: 2.8,
            crop_factor: 1.0,
            distortion: Correction {
                enabled: true,
                amount,
            },
            tca: Correction {
                enabled: true,
                amount,
            },
            vignetting: Correction {
                enabled: true,
                amount,
            },
        };

        let mut source = EditDoc::default();
        source.lens = Some(lens("sony-fe-16-35", 16.0, 0.8));
        let mut target = EditDoc::default();
        target.lens = Some(lens("nikon-35mm", 35.0, 0.2));

        let out = EditPatch::from_doc(&source, GroupSet::LENS).apply_to(&target);
        let got = out.lens.expect("target keeps its lens");

        assert_eq!(got.distortion.amount, 0.8, "amount must travel");
        assert_eq!(got.tca.amount, 0.8, "amount must travel");
        assert_eq!(got.vignetting.amount, 0.8, "amount must travel");
        assert_eq!(
            got.lens_id.as_deref(),
            Some("nikon-35mm"),
            "lens_id must NOT travel"
        );
        assert_eq!(got.focal_len, 35.0, "focal_len must NOT travel");
        assert_eq!(got.aperture, 2.8);
        assert_eq!(got.crop_factor, 1.0);
    }

    /// An unmatched target has no context to attach amounts to — no-op, no panic.
    #[test]
    // default-then-assign mirrors the plan's literal test spec; clearer than
    // struct-update for single fields.
    #[allow(clippy::field_reassign_with_default)]
    fn lens_group_is_a_noop_when_the_target_has_no_lens() {
        use crate::op::{Correction, LensCorrection};
        let mut source = EditDoc::default();
        source.lens = Some(LensCorrection {
            lens_id: Some("x".into()),
            focal_len: 16.0,
            aperture: 2.8,
            crop_factor: 1.0,
            distortion: Correction {
                enabled: true,
                amount: 1.0,
            },
            tca: Correction {
                enabled: true,
                amount: 1.0,
            },
            vignetting: Correction {
                enabled: true,
                amount: 1.0,
            },
        });
        let target = EditDoc::default(); // lens: None
        let out = EditPatch::from_doc(&source, GroupSet::LENS).apply_to(&target);
        assert!(out.lens.is_none(), "must not fabricate a LensCorrection");
    }

    /// A patch owning LIGHT with exposure == 0.0 must SET the target's exposure
    /// to 0, not skip it. If this ever fails, `owns` has been replaced by an
    /// is-identity check somewhere and presets can no longer reset a control.
    #[test]
    fn an_owned_identity_value_still_overwrites() {
        let source = EditDoc::default(); // exposure == 0.0
        let mut target = EditDoc::default();
        target.global.exposure = 2.0;

        let out = EditPatch::from_doc(&source, GroupSet::LIGHT).apply_to(&target);
        assert_eq!(
            out.global.exposure, 0.0,
            "identity value must still be written"
        );
    }

    /// MASKS is never honored in P7 — a patch claiming it must not touch layers.
    #[test]
    // default-then-assign mirrors the plan's literal test spec; clearer than
    // struct-update for single fields.
    #[allow(clippy::field_reassign_with_default)]
    fn masks_group_never_modifies_layers() {
        use crate::local::MaskLayer;
        let source = EditDoc::default();
        let mut target = EditDoc::default();
        target.layers = vec![MaskLayer {
            name: "Sky".into(),
            visible: true,
            mask: Default::default(),
            adjustments: AdjustmentSet::default(),
        }];
        let out = EditPatch::from_doc(&source, GroupSet::MASKS).apply_to(&target);
        assert_eq!(out.layers.len(), 1, "target's mask layers must survive");
        assert_eq!(out.layers[0].name, "Sky");
    }

    /// An empty patch is the identity transform.
    #[test]
    fn empty_patch_returns_the_target_unchanged() {
        let mut source = EditDoc::default();
        source.global.exposure = 9.0;
        let mut target = EditDoc::default();
        target.global.exposure = 1.0;
        let out = EditPatch::from_doc(&source, GroupSet::EMPTY).apply_to(&target);
        assert_eq!(out, target);
    }

    /// F8 (whole-branch review): spec §8.1 asks for a table-driven test —
    /// for EVERY group in `GroupSet::ALL_APPLICABLE`, a patch owning only
    /// that group overwrites its own fields and leaves every other field of
    /// the target byte-identical. Before this, only LIGHT and COLOR had a
    /// dedicated owned-branch test; CURVE, HSL, GRADING, DETAIL, EFFECTS had
    /// none (all nine branches were verified correct by reading, so this is
    /// a coverage gap, not a defect — but it guards the one function the
    /// entire phase routes through).
    ///
    /// Strategy: build `target` as a per-group BASELINE (`group_test_baseline`
    /// — plain `EditDoc::default()`, except LENS needs a pre-existing matched
    /// `LensCorrection` for `apply_lens_amounts` to have anything to write
    /// into) and `changed` as that same baseline with ONLY group `g`'s fields
    /// set to a distinguishable value (`tag_group_fields`). Since `changed`
    /// differs from `target` in EXACTLY group `g`'s fields, a patch owning
    /// only `g` applied to `target` must produce a document byte-identical
    /// to `changed` — one equality assertion verifies both halves (owned
    /// fields overwrite; every unowned field survives).
    #[test]
    fn every_applicable_group_owns_only_its_own_fields() {
        for &group in GroupSet::ALL_APPLICABLE.iter() {
            let target = group_test_baseline(group);
            let changed = tag_group_fields(&target, group);
            let patch = EditPatch::from_doc(&changed, group);
            let out = patch.apply_to(&target);
            assert_eq!(
                out,
                changed,
                "group bits {:#x}: owned fields must overwrite AND every unowned \
                 field must survive byte-identical",
                group.bits()
            );
        }
    }

    /// Per-group baseline target for `every_applicable_group_owns_only_its_own_fields`.
    /// Every group uses `EditDoc::default()` except LENS, which needs a
    /// pre-existing matched `LensCorrection` — `apply_lens_amounts` is a
    /// documented no-op when the target has none (see
    /// `lens_group_is_a_noop_when_the_target_has_no_lens` above), so an
    /// all-default target would make the LENS case of the table vacuous.
    fn group_test_baseline(group: GroupSet) -> EditDoc {
        let mut doc = EditDoc::default();
        if group == GroupSet::LENS {
            use crate::op::{Correction, LensCorrection};
            doc.lens = Some(LensCorrection {
                lens_id: Some("baseline-lens".to_string()),
                focal_len: 35.0,
                aperture: 2.8,
                crop_factor: 1.0,
                distortion: Correction::default(),
                tca: Correction::default(),
                vignetting: Correction::default(),
            });
        }
        doc
    }

    /// Returns a clone of `base` with ONLY `group`'s fields changed to a
    /// value distinguishable from `base`'s (always `group_test_baseline`'s
    /// identity/default value for that field).
    fn tag_group_fields(base: &EditDoc, group: GroupSet) -> EditDoc {
        let mut d = base.clone();
        if group == GroupSet::LIGHT {
            d.global.exposure = 1.5;
            d.global.contrast = 0.6;
            d.global.highlights = -0.3;
            d.global.shadows = 0.2;
            d.global.whites = 0.4;
            d.global.blacks = -0.1;
        } else if group == GroupSet::COLOR {
            d.global.temp = 0.25;
            d.global.tint = -0.1;
            d.global.saturation = 0.5;
            d.global.hue = 0.15;
            d.global.vibrance = 0.3;
            d.global.color = crate::local::ColorSwatch {
                r: 0.9,
                g: 0.1,
                b: 0.2,
                amount: 0.7,
            };
        } else if group == GroupSet::CURVE {
            d.global.tone_curve.points = vec![(0.0, 0.0), (0.5, 0.7), (1.0, 1.0)];
        } else if group == GroupSet::HSL {
            d.global.hsl.bands[2].hue = 0.4;
            d.global.hsl.bands[2].sat = -0.2;
        } else if group == GroupSet::GRADING {
            d.global.color_grade.shadows.hue = 210.0;
            d.global.color_grade.shadows.sat = 0.3;
        } else if group == GroupSet::DETAIL {
            d.global.sharpen.amount = 0.6;
            d.global.sharpen.radius = 3;
            d.global.noise_reduction.luminance = 0.4;
        } else if group == GroupSet::EFFECTS {
            d.global.dehaze.amount = 0.35;
        } else if group == GroupSet::GEOMETRY {
            d.geometry = Some(Geometry {
                crop: crate::op::CropRect {
                    x: 0.05,
                    y: 0.05,
                    w: 0.9,
                    h: 0.9,
                },
                angle_deg: 4.0,
                aspect: crate::op::Aspect::Square,
                keystone_v: 0.1,
                keystone_h: -0.05,
            });
        } else if group == GroupSet::LENS {
            use crate::op::Correction;
            let lens = d.lens.as_mut().expect("group_test_baseline set a lens");
            lens.distortion = Correction {
                enabled: true,
                amount: 0.6,
            };
            lens.tca = Correction {
                enabled: true,
                amount: 0.3,
            };
            lens.vignetting = Correction {
                enabled: true,
                amount: 0.5,
            };
        } else {
            panic!(
                "unhandled group in ALL_APPLICABLE table: bits {:#x}",
                group.bits()
            );
        }
        d
    }

    #[test]
    fn group_set_contains_insert_remove_roundtrip() {
        let mut g = GroupSet::EMPTY;
        assert!(g.is_empty());
        assert!(!g.contains(GroupSet::LIGHT));
        g.insert(GroupSet::LIGHT);
        g.insert(GroupSet::LENS);
        assert!(g.contains(GroupSet::LIGHT) && g.contains(GroupSet::LENS));
        assert!(!g.contains(GroupSet::COLOR));
        g.remove(GroupSet::LIGHT);
        assert!(!g.contains(GroupSet::LIGHT) && g.contains(GroupSet::LENS));
        assert_eq!(GroupSet::from_bits(g.bits()), g);
        // EMPTY is contained by nothing — guards the `other.0 != 0` clause.
        assert!(!g.contains(GroupSet::EMPTY));
    }
}
