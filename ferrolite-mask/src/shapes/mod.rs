//! Analytic per-pixel mask shape evaluators (zero halo). Each shape owns a
//! build-once compute pass writing a single-channel `R32Float` `MaskBuffer`.

mod color_range;
mod linear;
mod luma_range;
mod radial;

pub use color_range::{ColorRangePass, ColorRangeUniform, MAX_COLOR_SAMPLES};
pub use linear::{LinearGradientPass, LinearGradientUniform};
pub use luma_range::{LumaRangePass, LumaRangeUniform};
pub use radial::{RadialGradientPass, RadialGradientUniform};
