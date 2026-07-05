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

/// Per-mask point-op adjustments. All scalars are zero-identity; `Default` is the
/// no-op set. Serde uses `#[serde(default)]` on every field so a payload written
/// by an older/newer build (missing/extra fields) loads as identity for those.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct AdjustmentSet {
    #[serde(default)] pub exposure: f32,
    #[serde(default)] pub contrast: f32,
    #[serde(default)] pub highlights: f32,
    #[serde(default)] pub shadows: f32,
    #[serde(default)] pub whites: f32,
    #[serde(default)] pub blacks: f32,
    #[serde(default)] pub temp: f32,
    #[serde(default)] pub tint: f32,
    #[serde(default)] pub saturation: f32,
    #[serde(default)] pub hue: f32,
    #[serde(default)] pub color: ColorSwatch,
    // Reserved neighborhood locals — no shader in P1 (greyed in Plan 4's UI).
    #[serde(default)] pub texture: f32,
    #[serde(default)] pub clarity: f32,
    #[serde(default)] pub dehaze: f32,
    #[serde(default)] pub sharpness: f32,
    #[serde(default)] pub noise: f32,
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
            && self.color.amount == 0.0
    }

    /// New set with one Light control reset to identity (immutable per-control reset).
    pub fn reset_light(&self, c: LightControl) -> Self {
        let mut s = *self;
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
        let mut s = *self;
        match c {
            ColorControl::Temp => s.temp = 0.0,
            ColorControl::Tint => s.tint = 0.0,
            ColorControl::Saturation => s.saturation = 0.0,
            ColorControl::Hue => s.hue = 0.0,
            ColorControl::Color => s.color = ColorSwatch::default(),
        }
        s
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
        let a = AdjustmentSet { exposure: 0.5, contrast: 0.3, ..Default::default() };
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
        assert_eq!(
            a.reset_color(ColorControl::Color).color.amount,
            0.0
        );
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
        assert_eq!(
            serde_json::from_str::<LocalAdjustments>(&json).unwrap(),
            la
        );
    }
}
