//! ferrolite-mask — the engine-transferable, photo-agnostic mask machinery.
//! Permissive dependency graph (no copyleft, no model weights) so it lifts into
//! a game engine as a unit (map §3, D7). Grows module-by-module across P1 Plan 1.

mod buffer;
mod model;
mod vec;

pub use buffer::{MaskBuffer, MASK_FORMAT};
pub use model::{
    composite_scalar, BrushNode, CompositeMode, MaskComponent, MaskDefinition, MaskProvenance,
    RasterHandle, Stroke,
};
pub use vec::{Rgb, Vec2};
