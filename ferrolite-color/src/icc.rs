//! ICC profile emit/parse via `moxcms` (pure Rust). Profiles are emitted for
//! embedding on export; parse validates an embedded profile if one is present.
//! moxcms-only for Plan 1 (the lcms2 fallback is deferred — see the plan's
//! Global Constraints).

use crate::error::ColorError;
use crate::working_space::WorkingSpace;
use moxcms::ColorProfile;

/// Standard ICC profile bytes for `space`, for embedding on export.
pub fn emit_icc(space: WorkingSpace) -> Result<Vec<u8>, ColorError> {
    let profile = match space {
        WorkingSpace::Srgb => ColorProfile::new_srgb(),
        WorkingSpace::AdobeRgb => ColorProfile::new_adobe_rgb(),
        WorkingSpace::DisplayP3 => ColorProfile::new_display_p3(),
        WorkingSpace::Rec2020 => ColorProfile::new_bt2020(),
        WorkingSpace::ProPhoto => ColorProfile::new_pro_photo_rgb(),
    };
    profile.encode().map_err(|e| ColorError::Icc(e.to_string()))
}

/// Validate that `bytes` is a parseable ICC profile (spec §4.4 — parse an
/// embedded ICC if ever present).
pub fn parse_icc(bytes: &[u8]) -> Result<(), ColorError> {
    ColorProfile::new_from_slice(bytes)
        .map(|_| ())
        .map_err(|e| ColorError::Icc(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::working_space::WorkingSpace;

    #[test]
    fn emits_valid_icc_for_every_space() {
        for space in WorkingSpace::ALL {
            let bytes = emit_icc(space).unwrap_or_else(|e| panic!("{space:?}: {e}"));
            assert!(
                bytes.len() > 128,
                "{space:?}: profile too small ({} bytes)",
                bytes.len()
            );
            // ICC signature 'acsp' lives at header offset 36..40.
            assert_eq!(
                &bytes[36..40],
                b"acsp",
                "{space:?}: missing ICC 'acsp' signature"
            );
        }
    }

    #[test]
    fn emitted_profile_round_trips_through_parse() {
        for space in WorkingSpace::ALL {
            let bytes = emit_icc(space).expect("emit");
            assert!(parse_icc(&bytes).is_ok(), "{space:?} failed to parse back");
        }
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_icc(&[0u8; 8]).is_err());
    }
}
