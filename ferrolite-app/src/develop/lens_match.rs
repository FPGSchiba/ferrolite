//! Build a lens-match query from decode metadata, and hold the shared Lensfun
//! DB handle. The DB load itself is cheap-ish (bundled XML parse) but not free;
//! it happens ONCE at startup (never per-image), and a failure disables the
//! lens-correction section rather than retrying per open (CLAUDE.md rule 1: no
//! per-frame/per-open DB work on the UI thread).

use ferrolite_decode::Metadata;
use ferrolite_lens::LensQuery;
use ferrolite_pipeline::OpStack;

/// A query needs at least camera make/model + a focal length (the correction
/// model is focal-dependent). Aperture defaults to a mid value (f/8) when
/// absent — vignetting is the only correction that consumes aperture;
/// distortion/TCA don't, so a missing aperture must not block those.
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

/// Gate for the auto-match-on-open trigger (Spec 4.4 Task 14 Step 1): a
/// candidate should only be computed for a stack that has NEVER had a
/// `LensCorrection` op — i.e. a brand-new image, or one whose correction was
/// explicitly cleared/reset by the user. Once a `LensCorrection` op exists
/// (persisted or user-added this session), the auto-match must stay out of
/// the way: recomputing it would silently overwrite an explicit `Clear` or a
/// manually-picked-then-reset lens with a fresh guess. Pure so the on-open
/// wiring in `app.rs` can be unit-tested without a `LensfunDb`/`JobSystem`.
pub fn should_auto_match(stack: &OpStack) -> bool {
    stack.lens_correction().is_none()
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

    #[test]
    fn should_auto_match_true_for_a_fresh_stack() {
        assert!(should_auto_match(&OpStack::default()));
    }

    #[test]
    fn should_auto_match_false_once_a_lens_correction_op_exists() {
        use ferrolite_pipeline::{Correction, LensCorrection};
        let lc = LensCorrection {
            lens_id: None,
            focal_len: 24.0,
            aperture: 8.0,
            crop_factor: 1.0,
            distortion: Correction {
                enabled: true,
                amount: 1.0,
            },
            tca: Correction::default(),
            vignetting: Correction::default(),
        };
        let stack = OpStack::default().set_op(ferrolite_pipeline::Op::LensCorrection(lc));
        assert!(
            !should_auto_match(&stack),
            "an existing LensCorrection op (even user-enabled with no lens_id) \
             must not be silently overwritten by a fresh auto-match guess"
        );
    }
}
