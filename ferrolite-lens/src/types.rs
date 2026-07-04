//! Pure, GPU/UI-free data the pipeline and app consume. The only lensfun-facing
//! code is `backend.rs`; everything here is our own vocabulary so an upstream
//! `lensfun` API break is a one-file fix.

/// Warp-grid resolution (nodes per axis). Coarse; sampled bilinearly on the GPU.
pub const GRID_N: u32 = 129;
/// Radial vignette-gain LUT length.
pub const VIGNETTE_LEN: u32 = 256;

#[derive(Debug, thiserror::Error)]
pub enum LensError {
    #[error("lens database load failed: {0}")]
    DbLoad(String),
}

/// A resolved lens (from auto-match or the manual picker).
#[derive(Clone, Debug, PartialEq)]
pub struct LensMatch {
    /// Stable Lensfun lens key (the model string we persist + re-resolve on open).
    pub lens_id: String,
    /// Human label for the panel.
    pub display_name: String,
    /// Crop factor of the matched camera (from the DB), fed to the Modifier.
    pub crop_factor: f32,
}

/// EXIF-derived query used to auto-match a lens.
#[derive(Clone, Debug, PartialEq)]
pub struct LensQuery {
    pub camera_make: String,
    pub camera_model: String,
    pub lens_model: Option<String>,
    pub focal_len: f32,
    pub aperture: f32,
}

/// Coarse per-channel source-coordinate grid (normalized [0,1] image space).
/// `coords[y*n + x] = [rU,rV, gU,gV, bU,bV]` — R/G/B differ only for TCA.
#[derive(Clone, Debug, PartialEq)]
pub struct WarpGrid {
    pub n: u32,
    pub coords: Vec<[f32; 6]>,
    /// Max |source − dest| over the grid, in pixels at the baked dims → halo.
    pub max_disp: f32,
}

/// Radial vignette-correction gain: `radial[i]` is the multiplier at
/// normalized radius `i/(len-1)` from the image center.
#[derive(Clone, Debug, PartialEq)]
pub struct VignetteMap {
    pub radial: Vec<f32>,
}
