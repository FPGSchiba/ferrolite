//! The only module that names `lensfun`. `match_lens`/`bake_geometry`/
//! `bake_vignetting`/`find_lenses` are all real (Tasks 2–4 landed).

use crate::types::{LensError, LensMatch, LensQuery, VignetteMap, WarpGrid};
#[cfg(test)]
use crate::types::{GRID_N, VIGNETTE_LEN};

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

/// Build a modifier at coarse grid dims `n×n`, enable distortion + TCA, and
/// read back a real per-channel warp grid.
///
/// # Confirmed lensfun 0.7.0 API (see `.superpowers/sdd/u2-report.md`)
///
/// There is no single call that fills a combined distortion+TCA 6-float
/// buffer. `Modifier::apply_geometry_distortion` fills a 2-float-per-pixel
/// `[x,y]` buffer with the **distortion-only** remap (row-major,
/// `[x0,y0,x1,y1,...]`), while `Modifier::apply_subpixel_distortion` fills a
/// 6-float-per-pixel `[xR,yR,xG,yG,xB,yB]` buffer with the **TCA-only** remap
/// (green channel is always the untouched input coordinate — TCA is a shift
/// relative to green, not a full geometry warp).
///
/// Both calls share the exact same pixel<->normalized round-trip (same
/// `norm_scale`/`center_x`/`center_y`, same `x_start`/`y_start` convention;
/// confirmed by reading `Modifier::apply_geometry_distortion` and
/// `Modifier::apply_subpixel_distortion` in `lensfun`'s `src/modifier.rs`),
/// so the TCA-only shift `(xR,yR) - (xG,yG)` from `apply_subpixel_distortion`
/// is directly additive, in the same pixel-space units, to the distortion-only
/// source coord `d` from `apply_geometry_distortion`. We compose them:
/// `R_src = d + (r0 - g0)`, `G_src = d`, `B_src = d + (b0 - g0)`. If the lens
/// has no TCA calibration, `apply_subpixel_distortion` returns `false` and we
/// fall back to identity TCA (`R_src = G_src = B_src = d`) — distortion still
/// applies.
fn bake_geometry_impl(lens: &lensfun::Lens, crop: f32, focal: f32, n: u32) -> Option<WarpGrid> {
    let mut modifier = lensfun::Modifier::new(lens, focal, crop, n, n, true);
    let has_dist = modifier.enable_distortion_correction(lens);
    // `has_tca` tells us whether a real TCA calibration was found for this
    // lens/focal; we gate `apply_subpixel_distortion` on it below so we only
    // trust its output (vs. falling back to identity) when a calibration
    // genuinely exists.
    let has_tca = modifier.enable_tca_correction(lens);
    if !has_dist {
        return None; // no distortion model for this lens at this focal length
    }

    // `apply_geometry_distortion` fills `2 * n * n` floats: `[x0,y0,x1,y1,...]`
    // row-major over the requested `n×n` rectangle starting at (0,0).
    let mut remap = vec![0.0f32; 2 * (n as usize) * (n as usize)];
    if !modifier.apply_geometry_distortion(0.0, 0.0, n as usize, n as usize, &mut remap) {
        return None;
    }

    // `apply_subpixel_distortion` fills `6 * n * n` floats:
    // `[xR,yR,xG,yG,xB,yB,...]` over the same `n×n` rectangle. Returns
    // `false` when the lens has no TCA calibration at this focal length —
    // in that case `tca` stays `None` and every node falls back to identity.
    let mut subpix = vec![0.0f32; 6 * (n as usize) * (n as usize)];
    let tca_ok = has_tca
        && modifier.apply_subpixel_distortion(0.0, 0.0, n as usize, n as usize, &mut subpix);

    let mut coords = Vec::with_capacity((n * n) as usize);
    let mut max_disp = 0.0f32;
    let denom = (n - 1).max(1) as f32;
    for y in 0..n {
        for x in 0..n {
            let idx = y as usize * n as usize + x as usize;
            let off = 2 * idx;
            let (dx, dy) = (remap[off], remap[off + 1]);

            let (dr, db) = if tca_ok {
                let s = 6 * idx;
                let (xr, yr) = (subpix[s], subpix[s + 1]);
                let (xg, yg) = (subpix[s + 2], subpix[s + 3]);
                let (xb, yb) = (subpix[s + 4], subpix[s + 5]);
                ((xr - xg, yr - yg), (xb - xg, yb - yg))
            } else {
                ((0.0, 0.0), (0.0, 0.0))
            };

            let (rx, ry) = (dx + dr.0, dy + dr.1);
            let (bx, by) = (dx + db.0, dy + db.1);
            let norm = [
                rx / denom,
                ry / denom,
                dx / denom,
                dy / denom,
                bx / denom,
                by / denom,
            ];
            // max_disp tracks the distortion-only (green) channel, as before
            // — TCA offsets are sub-pixel and must not change the halo.
            let d = ((dx - x as f32).powi(2) + (dy - y as f32).powi(2)).sqrt();
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

/// Build a modifier over a square `dim x dim` image (`dim = 2*len - 1`, odd so
/// the exact center sits on a pixel) and sample the **correction** gain along
/// a horizontal radius from the center pixel out to the right edge.
///
/// # Confirmed lensfun 0.7.0 API (see `.superpowers/sdd/u2-report.md`)
///
/// There is no method that returns the vignetting gain LUT directly. The real
/// applicator is `Modifier::apply_color_modification_f32`, which multiplies a
/// caller-supplied pixel buffer by the per-pixel gain in place. We drive it
/// with a buffer pre-filled with `1.0` so the output *is* the gain, sampling
/// one physical pixel at a time (`width=1, rows=1`) at `(x, y_center)` for
/// `x` running from the center out to the edge — this is the real polynomial
/// evaluated by the real code path, not a re-derivation of the math.
///
/// `reverse=false` on the `Modifier` means `apply_color_modification_f32`
/// takes the `DeVignetting` branch (mirrors upstream `reverse=false` in
/// `mod-color.cpp`), i.e. it multiplies by `1/gain`, which *brightens* the
/// (real) darkened corners — the correction curve `bake_vignetting` is meant
/// to produce.
fn bake_vignetting_impl(
    lens: &lensfun::Lens,
    crop: f32,
    focal: f32,
    aperture: f32,
    len: u32,
) -> Option<VignetteMap> {
    if len < 2 {
        return None;
    }
    // Odd square canvas so the center radius sample sits exactly on a pixel
    // and the right edge sits exactly on the normalized r=1 inscribed circle.
    let dim = 2 * len - 1;
    let mut modifier = lensfun::Modifier::new(lens, focal, crop, dim, dim, false);
    if !modifier.enable_vignetting_correction(lens, aperture, 1000.0) {
        return None;
    }

    let center = (len - 1) as f32;
    let mut radial = Vec::with_capacity(len as usize);
    for i in 0..len {
        let x = center + i as f32;
        let mut px = [1.0f32];
        if !modifier.apply_color_modification_f32(&mut px, x, center, 1, 1, 1) {
            return None;
        }
        radial.push(px[0]);
    }
    if radial.iter().any(|g| !g.is_finite()) {
        return None;
    }
    Some(VignetteMap { radial })
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

    fn find_lenses(&self, camera_hint: &str, needle: &str) -> Vec<LensMatch> {
        // Real fuzzy search over the lens model name (`Database::find_lenses`
        // with `camera = None`). We deliberately do NOT resolve `camera_hint`
        // to a specific `Camera` and pass it through: `Database::find_lenses`
        // uses the camera only to hard-filter by mount compatibility, and a
        // maker hint like "Canon" resolves ambiguously to whichever camera
        // body `find_cameras` ranks first (e.g. a fixed-lens compact with a
        // body-specific mount no interchangeable lens matches) — that would
        // silently zero out results instead of narrowing them. The picker's
        // `camera_hint` is used to pick each hit's reported crop factor
        // (falling back to the lens's own calibration crop) rather than as a
        // hard filter.
        let camera = self
            .db
            .find_cameras(Some(camera_hint), "")
            .into_iter()
            .next();
        self.db
            .find_lenses(None, needle)
            .into_iter()
            .map(|lens| LensMatch {
                lens_id: lens.model.clone(),
                display_name: lens.model.clone(),
                crop_factor: camera.map(|c| c.crop_factor).unwrap_or(lens.crop_factor),
            })
            .collect()
    }
    fn bake_geometry(&self, m: &LensMatch, focal: f32, n: u32) -> Option<WarpGrid> {
        let lens = self.lens_by_id(&m.lens_id)?;
        bake_geometry_impl(lens, m.crop_factor, focal, n)
    }
    fn bake_vignetting(
        &self,
        m: &LensMatch,
        focal: f32,
        aperture: f32,
        len: u32,
    ) -> Option<VignetteMap> {
        let lens = self.lens_by_id(&m.lens_id)?;
        bake_vignetting_impl(lens, m.crop_factor, focal, aperture, len)
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
    fn bake_geometry_carries_real_per_channel_tca() {
        // Same lens/focal as the distortion test above: `slr-canon.xml` has a
        // real `<tca model="poly3" focal="24" .../>` calibration entry for
        // this exact lens, confirmed by reading the bundled DB source.
        let q = LensQuery {
            camera_make: "Canon".into(),
            camera_model: "Canon EOS 5D Mark III".into(),
            lens_model: Some("Canon EF 24-70mm f/2.8L II USM".into()),
            focal_len: 24.0,
            aperture: 8.0,
        };
        let db = db();
        let m = db.match_lens(&q).unwrap();
        let lens = db.lens_by_id(&m.lens_id).expect("lens resolves");
        assert!(
            lens.interpolate_tca(24.0).is_some(),
            "fixture lens must have a real TCA calibration at focal=24"
        );
        let g = db
            .bake_geometry(&m, 24.0, GRID_N)
            .expect("distortion model exists");

        // Pick an off-center node (not the exact center, where the TCA shift
        // is ~0 by construction) and assert R/G/B genuinely differ.
        let (x, y) = (GRID_N - 1, GRID_N / 2);
        let node = g.coords[(y * GRID_N + x) as usize];
        let (r, gc, b) = ([node[0], node[1]], [node[2], node[3]], [node[4], node[5]]);
        assert_ne!(
            r, gc,
            "red source coord must differ from green for a lens with real TCA data"
        );
        assert_ne!(
            b, gc,
            "blue source coord must differ from green for a lens with real TCA data"
        );
        assert_ne!(
            r, b,
            "red source coord must differ from blue for a lens with real TCA data"
        );
        assert!(node.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn bake_geometry_falls_back_to_identity_tca_when_lens_has_none() {
        // `Canon EF 17-35mm f/2.8L USM` has a real distortion calibration but
        // no `<tca>` entry at all in `slr-canon.xml` — confirmed by reading
        // the bundled DB source.
        let q = LensQuery {
            camera_make: "Canon".into(),
            camera_model: "Canon EOS 5D Mark III".into(),
            lens_model: Some("Canon EF 17-35mm f/2.8L USM".into()),
            focal_len: 17.0,
            aperture: 8.0,
        };
        let db = db();
        let m = db.match_lens(&q).unwrap();
        let lens = db.lens_by_id(&m.lens_id).expect("lens resolves");
        assert!(
            lens.interpolate_tca(17.0).is_none(),
            "fixture lens must NOT have a TCA calibration"
        );
        let g = db
            .bake_geometry(&m, 17.0, GRID_N)
            .expect("distortion model exists");
        for node in &g.coords {
            assert_eq!(
                [node[0], node[1]],
                [node[2], node[3]],
                "no TCA data: red must equal green"
            );
            assert_eq!(
                [node[4], node[5]],
                [node[2], node[3]],
                "no TCA data: blue must equal green"
            );
        }
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

    #[test]
    fn bake_vignetting_falls_off_toward_edges() {
        let q = LensQuery {
            camera_make: "Canon".into(),
            camera_model: "Canon EOS 5D Mark III".into(),
            lens_model: Some("Canon EF 24-70mm f/2.8L II USM".into()),
            focal_len: 24.0,
            aperture: 2.8, // wide open vignettes most
        };
        let db = db();
        let m = db.match_lens(&q).unwrap();
        let baked = db.bake_vignetting(&m, 24.0, 2.8, VIGNETTE_LEN);
        assert!(
            baked.is_some(),
            "expected a real vignetting calibration for this lens/aperture"
        );
        if let Some(v) = baked {
            assert_eq!(v.radial.len() as u32, VIGNETTE_LEN);
            assert!(v.radial.iter().all(|g| g.is_finite() && *g > 0.0));
            // Correction gain grows toward the edge (brightens the darkened corners).
            assert!(v.radial[VIGNETTE_LEN as usize - 1] >= v.radial[0]);
        }
    }

    #[test]
    fn find_lenses_search_returns_matches() {
        let hits = db().find_lenses("Canon", "24-70");
        assert!(hits.iter().any(|m| m.display_name.contains("24-70")));
    }
}
