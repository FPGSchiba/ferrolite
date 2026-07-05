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

/// Pure CPU reference for the mask compositing semantics (design §4.2). The WGSL
/// `mask_fold`/`mask_invert` passes mirror these operators exactly; the goldens
/// are validated against this. `components[i].0` is the i-th evaluated mask value
/// in `[0,1]`; the first seeds the accumulator, later entries fold by their mode.
/// Empty → `1.0` (full mask); `invert` applies `1 - m` last (empty+invert → 0.0).
pub fn composite_scalar(components: &[(f32, CompositeMode)], invert: bool) -> f32 {
    let mut acc = match components.first() {
        Some(&(v, _)) => v,
        None => 1.0,
    };
    for &(b, mode) in &components[components.len().min(1)..] {
        acc = match mode {
            CompositeMode::Add => acc.max(b),
            CompositeMode::Subtract => acc * (1.0 - b),
            CompositeMode::Intersect => acc.min(b),
        };
    }
    if invert {
        1.0 - acc
    } else {
        acc
    }
}

impl MaskDefinition {
    /// Composite pre-evaluated per-component `values` (one per component, same
    /// order) using each component's stored mode + `self.invert`.
    pub fn composite_scalar(&self, values: &[f32]) -> f32 {
        debug_assert_eq!(
            values.len(),
            self.components.len(),
            "one value per component required"
        );
        let pairs: Vec<(f32, CompositeMode)> = values
            .iter()
            .copied()
            .zip(self.components.iter().map(|(_, m)| *m))
            .collect();
        composite_scalar(&pairs, self.invert)
    }
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

    const M: f32 = 1e-6;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn empty_is_full_and_invert_is_empty() {
        assert!(approx(composite_scalar(&[], false), 1.0));
        assert!(approx(composite_scalar(&[], true), 0.0));
    }

    #[test]
    fn single_component_seeds_accumulator() {
        assert!(approx(
            composite_scalar(&[(0.42, CompositeMode::Add)], false),
            0.42
        ));
        // The seed's own mode is ignored — Subtract as the first entry still seeds.
        assert!(approx(
            composite_scalar(&[(0.42, CompositeMode::Subtract)], false),
            0.42
        ));
    }

    #[test]
    fn add_is_union_max() {
        let v = composite_scalar(
            &[(0.3, CompositeMode::Add), (0.7, CompositeMode::Add)],
            false,
        );
        assert!(approx(v, 0.7));
    }

    #[test]
    fn subtract_carves_out() {
        // 0.8 * (1 - 0.5) = 0.4
        let v = composite_scalar(
            &[(0.8, CompositeMode::Add), (0.5, CompositeMode::Subtract)],
            false,
        );
        assert!(approx(v, 0.4));
    }

    #[test]
    fn intersect_is_min() {
        let v = composite_scalar(
            &[(0.6, CompositeMode::Add), (0.25, CompositeMode::Intersect)],
            false,
        );
        assert!(approx(v, 0.25));
    }

    #[test]
    fn invert_flips_final_result() {
        let v = composite_scalar(&[(0.3, CompositeMode::Add)], true);
        assert!(approx(v, 0.7));
    }

    #[test]
    fn fold_is_left_to_right() {
        // seed 0.9, subtract 0.5 -> 0.45, intersect 0.2 -> 0.2
        let v = composite_scalar(
            &[
                (0.9, CompositeMode::Add),
                (0.5, CompositeMode::Subtract),
                (0.2, CompositeMode::Intersect),
            ],
            false,
        );
        assert!(approx(v, 0.2));
    }

    #[test]
    fn definition_helper_zips_values_with_modes() {
        let def = MaskDefinition {
            components: vec![
                (
                    MaskComponent::LumaRange {
                        lo: 0.0,
                        hi: 1.0,
                        softness: 0.0,
                    },
                    CompositeMode::Add,
                ),
                (
                    MaskComponent::LumaRange {
                        lo: 0.0,
                        hi: 1.0,
                        softness: 0.0,
                    },
                    CompositeMode::Subtract,
                ),
            ],
            invert: false,
        };
        // seed 1.0, subtract 0.25 -> 0.75
        assert!((def.composite_scalar(&[1.0, 0.25]) - 0.75).abs() < M);
    }
}
