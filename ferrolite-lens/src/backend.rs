//! The only module that names `lensfun`. Filled in Tasks 2–4.
#![allow(dead_code)]

use crate::types::{LensError, LensMatch, LensQuery, VignetteMap, WarpGrid};

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
    fn bake_geometry(&self, _m: &LensMatch, _focal: f32, _n: u32) -> Option<WarpGrid> {
        None // Task 3
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
}
