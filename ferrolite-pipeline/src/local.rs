//! The local-adjustments document sub-model: an ordered stack of `MaskLayer`s,
//! each pairing a parametric `MaskDefinition` (ferrolite-mask, engine tier) with a
//! per-mask Light+Color `AdjustmentSet` (photo tier). Pure data — `Clone`,
//! `PartialEq`, serde. Applied by `LocalAdjustmentsNode`; persisted in `frl:ops`.
//! Reserved neighborhood fields (texture/clarity/dehaze/sharpness/noise) are
//! carried for schema stability but have no shader in P1 (P3/P4 own them).

use serde::{Deserialize, Serialize};

use ferrolite_mask::MaskDefinition;

/// A color/tint overlay swatch. `amount` 0 = identity (no tint).
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct ColorSwatch {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub amount: f32,
}

/// Noise-reduction parameters (luminance + chroma). All zero-identity; no
/// shader yet (carried for schema stability — the V2 Effects tab shows the
/// sliders but they are not wired until their pass lands).
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct NoiseReduction {
    pub luminance: f32,
    pub detail: f32,
    pub color: f32,
    pub color_detail: f32,
}

impl NoiseReduction {
    /// True when every field is zero-identity — the gate the GPU node's
    /// passthrough and `nr_halo` both key off.
    pub fn is_identity(&self) -> bool {
        self.luminance == 0.0 && self.detail == 0.0 && self.color == 0.0 && self.color_detail == 0.0
    }
}

/// Per-mask point-op adjustments. All scalars are zero-identity; `Default` is the
/// no-op set. Serde uses `#[serde(default)]` on every field so a payload written
/// by an older/newer build (missing/extra fields) loads as identity for those.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct AdjustmentSet {
    #[serde(default)]
    pub exposure: f32,
    #[serde(default)]
    pub contrast: f32,
    #[serde(default)]
    pub highlights: f32,
    #[serde(default)]
    pub shadows: f32,
    #[serde(default)]
    pub whites: f32,
    #[serde(default)]
    pub blacks: f32,
    #[serde(default)]
    pub temp: f32,
    #[serde(default)]
    pub tint: f32,
    #[serde(default)]
    pub saturation: f32,
    #[serde(default)]
    pub hue: f32,
    #[serde(default)]
    pub color: ColorSwatch,
    // New in the unified model (design 2026-07-28 §2): the full parameter block,
    // shared verbatim between the global layer and every mask layer. All
    // zero-identity, all `#[serde(default)]` (schema-stable forward).
    #[serde(default)]
    pub vibrance: f32,
    #[serde(default)]
    pub tone_curve: crate::op::ToneCurve,
    #[serde(default)]
    pub hsl: crate::op::Hsl,
    #[serde(default)]
    pub color_grade: crate::op::ColorGrade,
    #[serde(default)]
    pub sharpen: crate::op::Sharpen,
    #[serde(default)]
    pub dehaze: crate::op::Dehaze,
    #[serde(default)]
    pub noise_reduction: NoiseReduction,
    // Reserved neighborhood locals — no shader yet (Phase 4 owns them).
    #[serde(default)]
    pub texture: f32,
    #[serde(default)]
    pub clarity: f32,
}

/// A single Light control (per-control reset target).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LightControl {
    Exposure,
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Blacks,
}

/// A single Color control (per-control reset target).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorControl {
    Temp,
    Tint,
    Saturation,
    Hue,
    Color,
}

impl AdjustmentSet {
    /// True when every point-op field is zero-identity (reserved fields ignored —
    /// they carry no shader so cannot change output in P1).
    pub fn is_identity(&self) -> bool {
        self.exposure == 0.0
            && self.contrast == 0.0
            && self.highlights == 0.0
            && self.shadows == 0.0
            && self.whites == 0.0
            && self.blacks == 0.0
            && self.temp == 0.0
            && self.tint == 0.0
            && self.saturation == 0.0
            && self.hue == 0.0
            && self.vibrance == 0.0
            && self.color.amount == 0.0
            && self.tone_curve.is_identity()
            && self.hsl.is_identity()
            && self.color_grade.is_identity()
            && self.sharpen.amount == 0.0
            && self.dehaze.is_identity()
            && self.noise_reduction.is_identity()
    }

    /// Copy with every identity-valued STRUCTURED field snapped to its exact
    /// `Default`, so identity edits stay byte-equal to a reset — the same
    /// invariant `EditDoc::set_op` maintains (is_identity, PartialEq-vs-default,
    /// and the serde hash agree). Scalars pass through (0.0 is already canonical).
    pub fn normalized(&self) -> Self {
        let mut s = self.clone();
        if s.dehaze.is_identity() {
            s.dehaze = crate::op::Dehaze::default();
        }
        if s.sharpen.amount == 0.0 {
            s.sharpen = crate::op::Sharpen::default();
        }
        if s.tone_curve.is_identity() {
            s.tone_curve = crate::op::ToneCurve::default();
        }
        if s.color_grade.is_identity() {
            s.color_grade = crate::op::ColorGrade::default();
        }
        if s.hsl.is_identity() {
            s.hsl = crate::op::Hsl::default();
        }
        s
    }

    /// New set with one Light control reset to identity (immutable per-control reset).
    pub fn reset_light(&self, c: LightControl) -> Self {
        let mut s = self.clone();
        match c {
            LightControl::Exposure => s.exposure = 0.0,
            LightControl::Contrast => s.contrast = 0.0,
            LightControl::Highlights => s.highlights = 0.0,
            LightControl::Shadows => s.shadows = 0.0,
            LightControl::Whites => s.whites = 0.0,
            LightControl::Blacks => s.blacks = 0.0,
        }
        s
    }

    /// New set with one Color control reset to identity.
    pub fn reset_color(&self, c: ColorControl) -> Self {
        let mut s = self.clone();
        match c {
            ColorControl::Temp => s.temp = 0.0,
            ColorControl::Tint => s.tint = 0.0,
            ColorControl::Saturation => s.saturation = 0.0,
            ColorControl::Hue => s.hue = 0.0,
            ColorControl::Color => s.color = ColorSwatch::default(),
        }
        s
    }

    /// Copy carrying ONLY the fused engine's Light-stage fields (exposure,
    /// highlights/shadows/whites/blacks, temp/tint, contrast); every other
    /// field is reset to its identity `Default`. The exact complement of
    /// `color_segment` — together they partition every `AdjustmentSet` field
    /// exactly once (fields belonging to neither engine segment, e.g. sharpen/
    /// dehaze/noise_reduction/texture/clarity, are identity in both).
    pub fn light_segment(&self) -> Self {
        Self {
            exposure: self.exposure,
            contrast: self.contrast,
            highlights: self.highlights,
            shadows: self.shadows,
            whites: self.whites,
            blacks: self.blacks,
            temp: self.temp,
            tint: self.tint,
            ..Self::default()
        }
    }

    /// Copy carrying ONLY the fused engine's Color-stage fields (saturation,
    /// hue, vibrance, color swatch, tone curve, HSL, color grade); every other
    /// field is reset to its identity `Default`. The exact complement of
    /// `light_segment`.
    pub fn color_segment(&self) -> Self {
        Self {
            saturation: self.saturation,
            hue: self.hue,
            color: self.color,
            vibrance: self.vibrance,
            tone_curve: self.tone_curve.clone(),
            hsl: self.hsl,
            color_grade: self.color_grade,
            ..Self::default()
        }
    }
}

/// One mask + its adjustments. `MaskDefinition` is the engine-tier parametric mask
/// (source of truth); `adjustments` is what applies through it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct MaskLayer {
    pub name: String,
    pub visible: bool,
    #[serde(default)]
    pub mask: MaskDefinition,
    #[serde(default)]
    pub adjustments: AdjustmentSet,
}

/// The `Op::LocalAdjustments` payload: an ordered stack of mask layers applied as a
/// single pipeline stage (design §13 — N masks inside one op).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct LocalAdjustments {
    #[serde(default)]
    pub layers: Vec<MaskLayer>,
}

impl LocalAdjustments {
    /// Visible layers, in stack order (the only ones that affect output).
    pub fn visible_layers(&self) -> impl Iterator<Item = &MaskLayer> {
        self.layers.iter().filter(|l| l.visible)
    }

    /// True when no visible layer would change the image (empty, all hidden, or every
    /// visible layer is an identity adjustment).
    pub fn is_identity(&self) -> bool {
        self.visible_layers().all(|l| l.adjustments.is_identity())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_mask::{CompositeMode, MaskComponent, MaskDefinition, Vec2 as MVec2};

    #[test]
    fn adjustment_set_default_is_identity() {
        let a = AdjustmentSet::default();
        assert!(a.is_identity());
        assert_eq!(a.exposure, 0.0);
        assert_eq!(a.color.amount, 0.0);
    }

    #[test]
    fn reset_light_zeroes_one_control_only() {
        let a = AdjustmentSet {
            exposure: 0.5,
            contrast: 0.3,
            ..Default::default()
        };
        let r = a.reset_light(LightControl::Exposure);
        assert_eq!(r.exposure, 0.0, "exposure reset");
        assert_eq!(r.contrast, 0.3, "contrast untouched");
    }

    #[test]
    fn reset_color_zeroes_one_control_only() {
        let a = AdjustmentSet {
            temp: 0.4,
            saturation: -0.2,
            color: ColorSwatch {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                amount: 0.5,
            },
            ..Default::default()
        };
        assert_eq!(a.reset_color(ColorControl::Temp).temp, 0.0);
        assert_eq!(a.reset_color(ColorControl::Temp).saturation, -0.2);
        assert_eq!(a.reset_color(ColorControl::Color).color.amount, 0.0);
    }

    #[test]
    fn local_adjustments_default_is_identity() {
        assert!(LocalAdjustments::default().is_identity());
    }

    #[test]
    fn only_visible_layers_are_iterated() {
        let hidden = MaskLayer {
            name: "a".into(),
            visible: false,
            mask: MaskDefinition::default(),
            adjustments: AdjustmentSet {
                exposure: 1.0,
                ..Default::default()
            },
        };
        let shown = MaskLayer {
            name: "b".into(),
            visible: true,
            mask: MaskDefinition::default(),
            adjustments: AdjustmentSet {
                exposure: 1.0,
                ..Default::default()
            },
        };
        let la = LocalAdjustments {
            layers: vec![hidden, shown],
        };
        let names: Vec<&str> = la.visible_layers().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["b"]);
        assert!(
            !la.is_identity(),
            "one visible non-identity layer is not identity"
        );
    }

    #[test]
    fn model_round_trips_through_json() {
        let la = LocalAdjustments {
            layers: vec![MaskLayer {
                name: "sky".into(),
                visible: true,
                mask: MaskDefinition {
                    components: vec![(
                        MaskComponent::LinearGradient {
                            start: MVec2::new(0.0, 0.0),
                            end: MVec2::new(0.0, 1.0),
                        },
                        CompositeMode::Add,
                    )],
                    invert: false,
                },
                adjustments: AdjustmentSet {
                    exposure: -0.5,
                    temp: 0.3,
                    color: ColorSwatch {
                        r: 0.2,
                        g: 0.4,
                        b: 0.9,
                        amount: 0.25,
                    },
                    ..Default::default()
                },
            }],
        };
        let json = serde_json::to_string(&la).unwrap();
        assert_eq!(serde_json::from_str::<LocalAdjustments>(&json).unwrap(), la);
    }

    #[test]
    fn expanded_set_default_is_identity_and_serde_defaults_hold() {
        let s = AdjustmentSet::default();
        assert!(s.is_identity());
        // A payload written by an older build (missing every new field) loads as identity.
        let old_json = r#"{"exposure":0.0}"#;
        let parsed: AdjustmentSet = serde_json::from_str(old_json).unwrap();
        assert!(parsed.is_identity());
        assert_eq!(parsed, AdjustmentSet::default());
    }

    #[test]
    // default-then-assign mirrors the plan's literal test spec; clearer than
    // struct-update for single fields.
    #[allow(clippy::field_reassign_with_default)]
    fn each_structured_field_breaks_identity() {
        let mut s = AdjustmentSet::default();
        s.tone_curve.points = vec![(0.0, 0.1), (1.0, 1.0)];
        assert!(!s.is_identity(), "tone curve");

        let mut s = AdjustmentSet::default();
        s.hsl.bands[0].sat = 0.3;
        assert!(!s.is_identity(), "hsl");

        let mut s = AdjustmentSet::default();
        s.color_grade.shadows.sat = 0.4;
        assert!(!s.is_identity(), "color grade");

        let mut s = AdjustmentSet::default();
        s.sharpen.amount = 0.5;
        assert!(!s.is_identity(), "sharpen");

        let mut s = AdjustmentSet::default();
        s.dehaze.amount = 0.2;
        assert!(!s.is_identity(), "dehaze");

        let mut s = AdjustmentSet::default();
        s.noise_reduction.luminance = 0.5;
        assert!(!s.is_identity(), "noise reduction");

        let mut s = AdjustmentSet::default();
        s.vibrance = 0.1;
        assert!(!s.is_identity(), "vibrance");
    }

    #[test]
    // default-then-assign mirrors the plan's literal test spec; clearer than
    // struct-update for single fields.
    #[allow(clippy::field_reassign_with_default)]
    fn expanded_set_round_trips() {
        let mut s = AdjustmentSet::default();
        s.exposure = 0.5;
        s.tone_curve.points = vec![(0.0, 0.0), (0.4, 0.6), (1.0, 1.0)];
        s.hsl.bands[3].hue = -0.2;
        s.sharpen = crate::op::Sharpen {
            amount: 0.8,
            radius: 2,
        };
        s.dehaze.amount = -0.3;
        let json = serde_json::to_string(&s).unwrap();
        let back: AdjustmentSet = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
