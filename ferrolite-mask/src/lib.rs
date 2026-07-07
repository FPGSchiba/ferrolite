//! ferrolite-mask — the engine-transferable, photo-agnostic mask machinery.
//! Permissive dependency graph (no copyleft, no model weights) so it lifts into
//! a game engine as a unit (map §3, D7). Grows module-by-module across P1 Plan 1.

mod brush;
mod buffer;
mod composite;
mod compositor;
mod model;
mod pass;
mod raster_store;
mod shapes;
mod stroke;
mod tile_transform;
mod vec;

pub use brush::BrushRasterizer;
pub use buffer::{MaskBuffer, MASK_FORMAT};
pub use composite::{CompositeNode, CompositePass};
pub use compositor::{read_mask_r32f, ComponentCache, MaskCompositor};
pub use model::{
    composite_scalar, BrushNode, CompositeMode, MaskComponent, MaskDefinition, MaskProvenance,
    RasterHandle, Stroke,
};
pub use raster_store::RasterStore;
pub use shapes::{
    ColorRangePass, ColorRangeUniform, LinearGradientPass, LinearGradientUniform, LumaRangePass,
    LumaRangeUniform, RadialGradientPass, RadialGradientUniform, MAX_COLOR_SAMPLES,
};
pub use stroke::{
    composite_dabs, dab_alpha, halo_px, max_dab_radius, stroke_dabs, Dab, StrokeCursor,
    SPACING_FRAC,
};
pub use tile_transform::TileTransform;
pub use vec::{Rgb, Vec2};
