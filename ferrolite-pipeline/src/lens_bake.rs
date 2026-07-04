//! Shared, off-thread lens-bake primitive: turn a persisted [`LensCorrection`]
//! plus a [`LensDb`] into the GPU bake products (a warp grid for distortion/TCA
//! and a radial-gain LUT for vignetting). This is the ONE place the resolve,
//! crop-override, and conditional-bake logic lives, so the viewer (app), the
//! edited thumbnail (app), and the export path all produce identical corrections.
//!
//! CLAUDE.md rule 1: baking is CPU-heavy (XML-derived polynomial evaluation over
//! a 129×129 grid + a 256-entry radial LUT), so callers MUST invoke this from an
//! off-thread job — never the UI/update thread. It is a pure function of its
//! inputs, so it is trivially safe to call from any worker thread.

use ferrolite_lens::{LensDb, VignetteMap, WarpGrid, GRID_N, VIGNETTE_LEN};

use crate::op::LensCorrection;

/// Resolve `lc.lens_id` via [`LensDb::match_by_id`] and bake the correction
/// products for it. Returns `(warp, vignette)`, each `None` when the
/// corresponding correction is disabled, the lens id is absent/unresolvable, or
/// the matched lens has no calibration for it — all of which render as identity.
///
/// The match's own `crop_factor` (from the lens's OWN calibration, with no
/// camera context) is OVERRIDDEN with `lc.crop_factor`, the authoritative crop
/// persisted at match time from the actual shooting camera body. This keeps the
/// bake coherent with the rebuild trigger, which fingerprints `lc.crop_factor`
/// (matches `spawn_lens_bake`'s coherence fix, commit 9326e24).
///
/// Geometry (warp) bakes only when `distortion` or `tca` is enabled; vignetting
/// bakes only when `vignetting` is enabled. An Amount-only change touches neither
/// enabled flag nor the lens key, so callers gate re-baking on those and apply
/// amount changes as uniform-only updates instead (never a re-bake).
pub fn bake_products(
    db: &dyn LensDb,
    lc: &LensCorrection,
) -> (Option<WarpGrid>, Option<VignetteMap>) {
    let Some(m) = lc.lens_id.as_deref().and_then(|id| db.match_by_id(id)) else {
        return (None, None);
    };
    // Override the fallback calibration crop with the authoritative persisted one.
    let m = ferrolite_lens::LensMatch {
        crop_factor: lc.crop_factor,
        ..m
    };
    let warp = if lc.distortion.enabled || lc.tca.enabled {
        db.bake_geometry(&m, lc.focal_len, GRID_N)
    } else {
        None
    };
    let vignette = if lc.vignetting.enabled {
        db.bake_vignetting(&m, lc.focal_len, lc.aperture, VIGNETTE_LEN)
    } else {
        None
    };
    (warp, vignette)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::Correction;
    use ferrolite_lens::{LensCaps, LensMatch, LensQuery};

    fn lc(lens_id: Option<&str>, dist: bool, tca: bool, vig: bool) -> LensCorrection {
        LensCorrection {
            lens_id: lens_id.map(String::from),
            focal_len: 24.0,
            aperture: 8.0,
            crop_factor: 1.0,
            distortion: Correction {
                enabled: dist,
                amount: 1.0,
            },
            tca: Correction {
                enabled: tca,
                amount: 1.0,
            },
            vignetting: Correction {
                enabled: vig,
                amount: 1.0,
            },
        }
    }

    /// A `LensDb` stub that records the `crop_factor` it was asked to bake with,
    /// so the crop-override coherence fix can be asserted without a real db.
    struct SpyDb {
        seen_crop: std::cell::Cell<Option<f32>>,
    }
    impl LensDb for SpyDb {
        fn match_lens(&self, _q: &LensQuery) -> Option<LensMatch> {
            None
        }
        fn find_lenses(&self, _camera_hint: &str, _needle: &str) -> Vec<LensMatch> {
            Vec::new()
        }
        fn match_by_id(&self, lens_id: &str) -> Option<LensMatch> {
            Some(LensMatch {
                lens_id: lens_id.to_string(),
                display_name: lens_id.to_string(),
                // A DIFFERENT crop than lc.crop_factor so the override is visible.
                crop_factor: 99.0,
            })
        }
        fn bake_geometry(&self, m: &LensMatch, _focal: f32, _n: u32) -> Option<WarpGrid> {
            self.seen_crop.set(Some(m.crop_factor));
            None
        }
        fn bake_vignetting(
            &self,
            m: &LensMatch,
            _focal: f32,
            _aperture: f32,
            _len: u32,
        ) -> Option<VignetteMap> {
            self.seen_crop.set(Some(m.crop_factor));
            None
        }
        fn lens_caps(&self, _lens_id: &str, _focal: f32, _aperture: f32) -> Option<LensCaps> {
            // Not exercised by these bake-routing tests; the app-side FB2
            // panel is what actually calls `lens_caps`.
            None
        }
    }

    #[test]
    fn no_lens_id_yields_all_none() {
        let db = SpyDb {
            seen_crop: std::cell::Cell::new(None),
        };
        let (w, v) = bake_products(&db, &lc(None, true, true, true));
        assert!(w.is_none() && v.is_none());
        assert_eq!(db.seen_crop.get(), None, "no bake should be attempted");
    }

    #[test]
    fn all_disabled_bakes_nothing_even_with_lens() {
        let db = SpyDb {
            seen_crop: std::cell::Cell::new(None),
        };
        let (w, v) = bake_products(&db, &lc(Some("EF 24-70"), false, false, false));
        assert!(w.is_none() && v.is_none());
        assert_eq!(
            db.seen_crop.get(),
            None,
            "all-disabled correction must not touch the db"
        );
    }

    #[test]
    fn overrides_match_crop_with_lc_crop() {
        let db = SpyDb {
            seen_crop: std::cell::Cell::new(None),
        };
        let mut c = lc(Some("EF 24-70"), true, false, false);
        c.crop_factor = 1.6; // authoritative persisted crop
        let _ = bake_products(&db, &c);
        assert_eq!(
            db.seen_crop.get(),
            Some(1.6),
            "bake must use lc.crop_factor, not the match's own 99.0"
        );
    }
}
