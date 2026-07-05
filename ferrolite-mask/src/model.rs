//! The parametric mask vocabulary — the source of truth for a mask. Pure data:
//! `Clone`, `PartialEq`, and (de)serializable. Shapes are defined in normalized
//! source coordinates so masks stay anchored to image content across geometry.
//! `Brush` and `Imported` are inert data variants in P1 (no producer): the brush
//! rasterizer lands in Plan 2, the AI producer in A2.

use serde::{Deserialize, Serialize};

use crate::vec::{Rgb, Vec2};

/// How a component folds into the mask accumulator (design §4.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum CompositeMode {
    /// Union: `max(acc, b)`.
    #[default]
    Add,
    /// `acc * (1 - b)`.
    Subtract,
    /// `min(acc, b)`.
    Intersect,
}

/// A single brush node (inert in P1; rasterizer arrives in Plan 2).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct BrushNode {
    pub pos: Vec2,
    pub radius: f32,
    pub hardness: f32,
    pub flow: f32,
}

/// A brush stroke = an ordered polyline of dabs (inert in P1).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Stroke {
    pub nodes: Vec<BrushNode>,
    pub erase: bool,
}

/// Opaque handle to an externally-produced raster mask (the AI seam). Inert in P1.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RasterHandle(pub u64);

/// Engine-opaque descriptor for an imported (AI) mask. The engine stores but
/// never interprets it; A2 re-derives the raster from `prompt` (contract 2).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MaskProvenance {
    pub model_id: String,
    pub model_version: String,
    pub prompt: String,
}

/// One parametric mask component. All spatial params are in normalized source
/// coordinates ([0,1]²).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum MaskComponent {
    /// Linear ramp: mask = clamped projection of the pixel onto the start→end axis.
    LinearGradient { start: Vec2, end: Vec2 },
    /// Ellipse falloff centred at `center` with per-axis `radius`, rotated
    /// `rotation` radians, edge softened over `feather`.
    RadialGradient {
        center: Vec2,
        radius: Vec2,
        rotation: f32,
        feather: f32,
        invert: bool,
    },
    /// Smooth band over input luma in [lo, hi] with `softness` edges.
    LumaRange { lo: f32, hi: f32, softness: f32 },
    /// Smooth color-distance selection around `samples` (linear RGB).
    ColorRange {
        samples: Vec<Rgb>,
        tolerance: f32,
        softness: f32,
    },
    /// Brush strokes (inert data in P1; rasterizer in Plan 2).
    Brush { strokes: Vec<Stroke> },
    /// Imported/AI raster (inert data in P1; producer in A2).
    Imported {
        handle: RasterHandle,
        provenance: MaskProvenance,
    },
}

/// An ordered stack of `(component, mode)` folded into one effective mask, with a
/// final `invert`. Empty = full mask (see `composite_scalar`).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct MaskDefinition {
    pub components: Vec<(MaskComponent, CompositeMode)>,
    pub invert: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_definition_default_is_empty_not_inverted() {
        let def = MaskDefinition::default();
        assert!(def.components.is_empty());
        assert!(!def.invert);
    }

    #[test]
    fn model_round_trips_through_json() {
        let def = MaskDefinition {
            components: vec![
                (
                    MaskComponent::LinearGradient {
                        start: Vec2::new(0.1, 0.2),
                        end: Vec2::new(0.8, 0.9),
                    },
                    CompositeMode::Add,
                ),
                (
                    MaskComponent::LumaRange {
                        lo: 0.2,
                        hi: 0.7,
                        softness: 0.1,
                    },
                    CompositeMode::Subtract,
                ),
                (
                    MaskComponent::Imported {
                        handle: RasterHandle(42),
                        provenance: MaskProvenance {
                            model_id: "sam2.1".into(),
                            model_version: "1.0".into(),
                            prompt: "click:0.5,0.5".into(),
                        },
                    },
                    CompositeMode::Intersect,
                ),
            ],
            invert: true,
        };
        let json = serde_json::to_string(&def).expect("serialize");
        let back: MaskDefinition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(def, back);
    }

    #[test]
    fn composite_mode_defaults_to_add() {
        assert_eq!(CompositeMode::default(), CompositeMode::Add);
    }
}
