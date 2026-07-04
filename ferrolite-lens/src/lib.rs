//! Lens-correction adapter over the pure-Rust `lensfun` crate. Photo tier.
//! Isolates the pre-alpha dependency behind our own types (`types`) and a
//! `LensDb` trait, so the pipeline/app never name `lensfun`.

mod backend;
mod types;

pub use backend::{load_bundled, LensDb, LensfunDb, MAX_LENS_HALO};
pub use types::{
    LensCaps, LensError, LensMatch, LensQuery, VignetteMap, WarpGrid, GRID_N, VIGNETTE_LEN,
};

/// Halo (pixels) a tiled lens-corrected pass must over-fetch. Ceil + capped.
pub fn lens_halo(g: &WarpGrid) -> u32 {
    (g.max_disp.ceil() as u32).min(MAX_LENS_HALO)
}
