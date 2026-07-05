//! ferrolite-mask — the engine-transferable, photo-agnostic mask machinery.
//! Permissive dependency graph (no copyleft, no model weights) so it lifts into
//! a game engine as a unit (map §3, D7). Grows module-by-module across P1 Plan 1.

mod buffer;
mod composite;
mod model;
mod pass;
mod shapes;
mod stroke;
mod vec;

pub use buffer::{MaskBuffer, MASK_FORMAT};
pub use composite::{CompositeNode, CompositePass};
pub use model::{
    composite_scalar, BrushNode, CompositeMode, MaskComponent, MaskDefinition, MaskProvenance,
    RasterHandle, Stroke,
};
pub use shapes::{
    ColorRangePass, ColorRangeUniform, LinearGradientPass, LinearGradientUniform, LumaRangePass,
    LumaRangeUniform, RadialGradientPass, RadialGradientUniform, MAX_COLOR_SAMPLES,
};
pub use stroke::{max_dab_radius, stroke_dabs, Dab, SPACING_FRAC};
pub use vec::{Rgb, Vec2};
