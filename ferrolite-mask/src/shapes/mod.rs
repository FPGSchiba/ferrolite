//! Analytic per-pixel mask shape evaluators (zero halo). Each shape owns a
//! build-once compute pass writing a single-channel `R32Float` `MaskBuffer`.

mod linear;
mod luma_range;
mod radial;

pub use linear::{LinearGradientPass, LinearGradientUniform};
pub use luma_range::{LumaRangePass, LumaRangeUniform};
pub use radial::{RadialGradientPass, RadialGradientUniform};
