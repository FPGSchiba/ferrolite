//! Analytic per-pixel mask shape evaluators (zero halo). Each shape owns a
//! build-once compute pass writing a single-channel `R32Float` `MaskBuffer`.

mod linear;

pub use linear::{LinearGradientPass, LinearGradientUniform};
