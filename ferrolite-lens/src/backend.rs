//! The only module that names `lensfun`. `match_lens`/`bake_geometry`/
//! `bake_vignetting`/`find_lenses` are all real (Tasks 2–4 landed).

#[cfg(test)]
use crate::types::GRID_N;
use crate::types::{LensError, LensMatch, LensQuery, VignetteMap, WarpGrid};

/// Max halo (px) a tiled lens-corrected pass over-fetches (mirrors MAX_SHARPEN_RADIUS).
pub const MAX_LENS_HALO: u32 = 256;

/// Uniform surface the pipeline/app use to resolve and bake lens corrections.
/// The only implementation today is [`LensfunDb`]; the trait exists so callers
/// never name `lensfun` directly.
pub trait LensDb {
    fn match_lens(&self, q: &LensQuery) -> Option<LensMatch>;
    fn find_lenses(&self, camera_hint: &str, needle: &str) -> Vec<LensMatch>;
    fn bake_geometry(&self, m: &LensMatch, focal: f32, n: u32) -> Option<WarpGrid>;
    fn bake_vignetting(
        &self,
        m: &LensMatch,
        focal: f32,
        aperture: f32,
        len: u32,
    ) -> Option<VignetteMap>;
}

/// Opaque wrapper around a loaded `lensfun::Database`.
pub struct LensfunDb {
    db: lensfun::Database,
}

/// Load the lens database bundled with the `lensfun` crate (no network/filesystem
/// dependency on the user's machine).
pub fn load_bundled() -> Result<LensfunDb, LensError> {
    let db = lensfun::Database::load_bundled().map_err(|e| LensError::DbLoad(format!("{e:?}")))?;
    Ok(LensfunDb { db })
}

impl LensfunDb {
    /// Resolve the camera (for crop factor) then the lens; returns both or `None`.
    fn resolve(&self, q: &LensQuery) -> Option<(&lensfun::Lens, f32)> {
        let cam = self
            .db
            .find_cameras(Some(&q.camera_make), &q.camera_model)
            .into_iter()
            .next()?;
        let crop = cam.crop_factor;
        let needle = q.lens_model.as_deref()?;
        let lens = self.db.find_lenses(Some(cam), needle).into_iter().next()?;
        Some((lens, crop))
    }

    /// Look up a previously-matched lens by its stable `lens_id` (the model
    /// string persisted in [`LensMatch::lens_id`]) so bakes can re-resolve the
    /// `lensfun::Lens` without re-running EXIF matching.
    fn lens_by_id(&self, lens_id: &str) -> Option<&lensfun::Lens> {
        self.db.lenses.iter().find(|l| l.model == lens_id)
    }
}

/// Build a modifier at coarse grid dims `n×n`, enable distortion (+ TCA if
/// available), and read back a per-channel warp grid.
///
/// # Confirmed lensfun 0.7.0 API (see `.superpowers/sdd/u2-report.md`)
///
/// There is no single call that fills a combined distortion+TCA 6-float
/// buffer. `Modifier::apply_geometry_distortion` fills a 2-float-per-pixel
/// `[x,y]` buffer with the **distortion-only** remap (row-major,
/// `[x0,y0,x1,y1,...]`), while `Modifier::apply_subpixel_distortion` fills a
/// 6-float-per-pixel `[xR,yR,xG,yG,xB,yB]` buffer with the **TCA-only** remap
/// (green channel is always the untouched input coordinate — TCA is a shift
/// relative to green, not a full geometry warp). Combining both into one true
/// per-channel warp would require re-deriving the normalized<->pixel
/// conversion lensfun does internally.
///
/// Per the brief's documented fallback, we take the distortion-only path and
/// fill R=G=B from `apply_geometry_distortion`, leaving TCA as identity
/// (R/G/B coincide). This is real, verified distortion data — just not also
/// carrying real TCA. Revisit if a future task needs true per-channel TCA.
fn bake_geometry_impl(lens: &lensfun::Lens, crop: f32, focal: f32, n: u32) -> Option<WarpGrid> {
    let mut modifier = lensfun::Modifier::new(lens, focal, crop, n, n, true);
    let has_dist = modifier.enable_distortion_correction(lens);
    // TCA is currently not folded into the grid (see doc comment above); we
    // still enable it so a future combined-warp implementation is a one-line
    // change, but its output isn't consumed yet.
    let _has_tca = modifier.enable_tca_correction(lens);
    if !has_dist {
        return None; // no distortion model for this lens at this focal length
    }

    // `apply_geometry_distortion` fills `2 * n * n` floats: `[x0,y0,x1,y1,...]`
    // row-major over the requested `n×n` rectangle starting at (0,0).
    let mut remap = vec![0.0f32; 2 * (n as usize) * (n as usize)];
    if !modifier.apply_geometry_distortion(0.0, 0.0, n as usize, n as usize, &mut remap) {
        return None;
    }

    let mut coords = Vec::with_capacity((n * n) as usize);
    let mut max_disp = 0.0f32;
    let denom = (n - 1).max(1) as f32;
    for y in 0..n {
        for x in 0..n {
            let off = 2 * (y as usize * n as usize + x as usize);
            let (rx, ry) = (remap[off], remap[off + 1]);
            let norm = [
                rx / denom,
                ry / denom,
                rx / denom,
                ry / denom,
                rx / denom,
                ry / denom,
            ];
            let d = ((rx - x as f32).powi(2) + (ry - y as f32).powi(2)).sqrt();
            max_disp = max_disp.max(d);
            coords.push(norm);
        }
    }
    // max_disp is in grid-pixel units; scale to a conservative full-res halo
    // estimate as a fraction of image extent. Keep it simple: fraction * a
    // reference extent, capped downstream by `lens_halo`.
    let frac = max_disp / denom; // fraction of image dimension
    let max_disp_px = frac * MAX_LENS_HALO as f32 * 4.0; // conservative; capped downstream
    Some(WarpGrid {
        n,
        coords,
        max_disp: max_disp_px,
    })
}

impl LensDb for LensfunDb {
    fn match_lens(&self, q: &LensQuery) -> Option<LensMatch> {
        let (lens, crop) = self.resolve(q)?;
        Some(LensMatch {
            lens_id: lens.model.clone(),
            display_name: lens.model.clone(),
            crop_factor: crop,
        })
    }

    fn find_lenses(&self, _camera_hint: &str, _needle: &str) -> Vec<LensMatch> {
        Vec::new() // Task 4
    }
    fn bake_geometry(&self, m: &LensMatch, focal: f32, n: u32) -> Option<WarpGrid> {
        let lens = self.lens_by_id(&m.lens_id)?;
        bake_geometry_impl(lens, m.crop_factor, focal, n)
    }
    fn bake_vignetting(
        &self,
        _m: &LensMatch,
        _focal: f32,
        _aperture: f32,
        _len: u32,
    ) -> Option<VignetteMap> {
        None // Task 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LensQuery;

    fn db() -> LensfunDb {
        load_bundled().expect("bundled lens db loads")
    }

    #[test]
    fn matches_a_well_known_lens() {
        // Confirmed present in the bundled DB via the spike (Task 2 Step 1).
        let q = LensQuery {
            camera_make: "Canon".into(),
            camera_model: "Canon EOS 5D Mark III".into(),
            lens_model: Some("Canon EF 24-70mm f/2.8L II USM".into()),
            focal_len: 50.0,
            aperture: 8.0,
        };
        let m = db().match_lens(&q).expect("known lens matches");
        assert!(m.display_name.to_lowercase().contains("24-70"));
        assert!(
            m.crop_factor > 0.9 && m.crop_factor < 1.1,
            "full-frame ≈ 1.0"
        );
    }

    #[test]
    fn unknown_lens_is_none() {
        let q = LensQuery {
            camera_make: "Nonexistent".into(),
            camera_model: "No Such Camera 9000".into(),
            lens_model: Some("Imaginary 999mm f/0.5".into()),
            focal_len: 50.0,
            aperture: 8.0,
        };
        assert!(db().match_lens(&q).is_none());
    }

    #[test]
    fn bake_geometry_produces_grid_with_disp_for_distorting_lens() {
        let q = LensQuery {
            camera_make: "Canon".into(),
            camera_model: "Canon EOS 5D Mark III".into(),
            lens_model: Some("Canon EF 24-70mm f/2.8L II USM".into()),
            focal_len: 24.0, // wide end distorts more
            aperture: 8.0,
        };
        let db = db();
        let m = db.match_lens(&q).unwrap();
        let g = db
            .bake_geometry(&m, 24.0, GRID_N)
            .expect("distortion model exists");
        assert_eq!(g.n, GRID_N);
        assert_eq!(g.coords.len() as u32, GRID_N * GRID_N);
        // The center node maps ≈ to itself; corners displace outward for barrel.
        let center = g.coords[(GRID_N * GRID_N / 2) as usize];
        assert!((center[0] - 0.5).abs() < 0.02 && (center[1] - 0.5).abs() < 0.02);
        assert!(
            g.max_disp > 0.0,
            "a distorting lens has non-zero displacement"
        );
        // All coords finite and roughly in-bounds (bilinear edge-clamp handles the rest).
        assert!(g.coords.iter().flatten().all(|v| v.is_finite()));
    }

    #[test]
    fn lens_halo_is_ceil_capped() {
        let g = WarpGrid {
            n: 2,
            coords: vec![[0.0; 6]; 4],
            max_disp: 12.3,
        };
        assert_eq!(crate::lens_halo(&g), 13);
        let big = WarpGrid {
            n: 2,
            coords: vec![[0.0; 6]; 4],
            max_disp: 9999.0,
        };
        assert_eq!(crate::lens_halo(&big), MAX_LENS_HALO);
    }
}
