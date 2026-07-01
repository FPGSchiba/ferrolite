//! ferrolite-color — pure, `unsafe`-free color math for ferrolite.
//!
//! Working-space definitions, RGB↔XYZ matrices, Bradford camera→working
//! adaptation, working→display / working→output transforms, sRGB transfer
//! functions, and ICC emit/parse via `moxcms`. No GPU, no UI, no `rawler`.
//! Photo tier (GPL-OK); the whole crate is unit-testable on every OS.

mod adapt;
mod camera;
mod error;
mod icc;
mod matrix;
mod oetf;
mod tail;
mod working_space;

pub use adapt::chromatic_adaptation;
pub use camera::camera_to_working;
// pub use error::ColorError;
// pub use icc::{emit_icc, parse_icc};
pub use matrix::{diag, identity, inverse, mul_mat3, mul_vec3, Mat3, Xy};
// pub use oetf::{srgb_eotf, srgb_oetf};
// pub use tail::{working_to_display, working_to_output};
pub use working_space::WorkingSpace;
