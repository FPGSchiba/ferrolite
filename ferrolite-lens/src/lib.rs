//! Lens-correction adapter over the pure-Rust `lensfun` crate. Photo tier.
//! Isolates the pre-alpha dependency behind our own types (`types`) and a
//! `LensDb` trait, so the pipeline/app never name `lensfun`.

mod backend;
mod types;

pub use backend::{load_bundled, LensDb, LensfunDb};
pub use types::{LensError, LensMatch, LensQuery, VignetteMap, WarpGrid, GRID_N, VIGNETTE_LEN};
