//! Build a lens-match query from decode metadata, and hold the shared Lensfun
//! DB handle. The DB load itself is cheap-ish (bundled XML parse) but not free;
//! it happens ONCE at startup (never per-image), and a failure disables the
//! lens-correction section rather than retrying per open (CLAUDE.md rule 1: no
//! per-frame/per-open DB work on the UI thread).

use ferrolite_decode::Metadata;
use ferrolite_lens::LensQuery;

/// A query needs at least camera make/model + a focal length (the correction
/// model is focal-dependent). Aperture defaults to a mid value (f/8) when
/// absent — vignetting is the only correction that consumes aperture;
/// distortion/TCA don't, so a missing aperture must not block those.
#[allow(dead_code)] // wired to the auto-match-on-open trigger + manual picker (later task)
pub fn query_from_metadata(m: &Metadata) -> Option<LensQuery> {
    let focal = m.focal_length?;
    Some(LensQuery {
        camera_make: m.make.clone(),
        camera_model: m.model.clone(),
        lens_model: m.lens.clone(),
        focal_len: focal,
        aperture: m.aperture.unwrap_or(8.0),
    })
}

/// Load the bundled Lensfun DB once. On failure, logs to stderr and returns
/// `None` — callers store this as `Option<Arc<LensfunDb>>` and disable the
/// lens-correction section (auto-match + bake) when it's `None`, rather than
/// retrying on every image open.
pub fn load_shared_db() -> Option<std::sync::Arc<ferrolite_lens::LensfunDb>> {
    match ferrolite_lens::load_bundled() {
        Ok(db) => Some(std::sync::Arc::new(db)),
        Err(e) => {
            eprintln!("ferrolite: lens DB load failed, lens correction disabled: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_decode::Metadata;
    use ferrolite_image::Orientation;

    fn meta(lens: Option<&str>, focal: Option<f32>, ap: Option<f32>) -> Metadata {
        Metadata {
            make: "Canon".into(),
            model: "Canon EOS 5D Mark III".into(),
            width: 5760,
            height: 3840,
            orientation: Orientation::Normal,
            iso: Some(100),
            aperture: ap,
            shutter: Some(0.01),
            focal_length: focal,
            capture_time: None,
            lens: lens.map(String::from),
        }
    }

    #[test]
    fn builds_query_when_focal_and_aperture_present() {
        let q = query_from_metadata(&meta(Some("EF 24-70"), Some(50.0), Some(8.0))).unwrap();
        assert_eq!(q.camera_make, "Canon");
        assert_eq!(q.focal_len, 50.0);
        assert_eq!(q.lens_model.as_deref(), Some("EF 24-70"));
    }

    #[test]
    fn none_without_focal_length() {
        // Focal length is required to build the correction model.
        assert!(query_from_metadata(&meta(Some("EF 24-70"), None, Some(8.0))).is_none());
    }

    #[test]
    fn defaults_aperture_when_absent() {
        let q = query_from_metadata(&meta(Some("EF 24-70"), Some(35.0), None)).unwrap();
        assert_eq!(q.aperture, 8.0);
    }

    #[test]
    fn carries_none_lens_model_when_exif_has_no_lens() {
        let q = query_from_metadata(&meta(None, Some(35.0), Some(4.0))).unwrap();
        assert_eq!(q.lens_model, None);
    }
}
