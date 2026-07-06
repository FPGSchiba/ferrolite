//! User-facing camera color-profile coverage status for the open image.
//!
//! Spec 4.6 §3: surface when a RAW decoded WITHOUT a usable camera color
//! matrix (sRGB fallback in effect) so the user knows the colors are
//! approximate. Pure + egui-free so the four-state derivation is unit-tested;
//! the adjustment panel renders the result.

use ferrolite_image::FileKind;

/// Camera color-profile coverage state for the open image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageStatus {
    /// Not a RAW (Standard raster) — no camera-matrix concept. Render nothing.
    NotApplicable,
    /// RAW, tier-2 full decode not yet complete — the stored profile is still
    /// the seed fallback and is not authoritative. Render nothing.
    Pending,
    /// RAW decoded WITH a real camera matrix.
    Calibrated,
    /// RAW decoded but no usable matrix — sRGB fallback in effect (the warning).
    Fallback,
}

impl CoverageStatus {
    /// Short warning-chip label, or `None` when no chip should be shown.
    pub fn chip_label(self) -> Option<&'static str> {
        match self {
            CoverageStatus::Fallback => Some("approximate color"),
            _ => None,
        }
    }

    /// Hover tooltip explaining the chip, or `None` when there is no chip.
    pub fn tooltip(self) -> Option<&'static str> {
        match self {
            CoverageStatus::Fallback => Some(
                "No color profile for this camera \u{2014} colors are approximate \
                 (sRGB fallback). Consider contributing a sample upstream to rawler.",
            ),
            _ => None,
        }
    }
}

/// Derive coverage status from the open image's kind, whether the tier-2 full
/// decode has completed (`full_ready`), and the decoded profile's fallback flag.
///
/// `is_fallback` alone is insufficient: it is `true` for the seed profile before
/// decode and meaningless for non-RAW rasters — see the unit tests.
pub fn camera_coverage(kind: FileKind, full_ready: bool, is_fallback: bool) -> CoverageStatus {
    match kind {
        FileKind::Standard => CoverageStatus::NotApplicable,
        FileKind::Raw if !full_ready => CoverageStatus::Pending,
        FileKind::Raw if is_fallback => CoverageStatus::Fallback,
        FileKind::Raw => CoverageStatus::Calibrated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_image::FileKind;

    #[test]
    fn standard_is_not_applicable() {
        // Non-RAW rasters have no camera-matrix concept — never warn, even
        // though the seed profile's is_fallback may be true.
        assert_eq!(
            camera_coverage(FileKind::Standard, true, true),
            CoverageStatus::NotApplicable
        );
        assert_eq!(
            camera_coverage(FileKind::Standard, false, false),
            CoverageStatus::NotApplicable
        );
    }

    #[test]
    fn raw_before_full_decode_is_pending() {
        // ViewerState.color_profile is the sRGB fallback (is_fallback == true)
        // until the tier-2 full decode arrives; must NOT warn during preview.
        assert_eq!(
            camera_coverage(FileKind::Raw, false, true),
            CoverageStatus::Pending
        );
    }

    #[test]
    fn raw_decoded_with_matrix_is_calibrated() {
        assert_eq!(
            camera_coverage(FileKind::Raw, true, false),
            CoverageStatus::Calibrated
        );
    }

    #[test]
    fn raw_decoded_without_matrix_is_fallback() {
        assert_eq!(
            camera_coverage(FileKind::Raw, true, true),
            CoverageStatus::Fallback
        );
    }

    #[test]
    fn only_fallback_shows_a_chip_and_tooltip() {
        assert!(CoverageStatus::Fallback.chip_label().is_some());
        assert!(CoverageStatus::Fallback.tooltip().is_some());
        for s in [
            CoverageStatus::NotApplicable,
            CoverageStatus::Pending,
            CoverageStatus::Calibrated,
        ] {
            assert!(s.chip_label().is_none());
            assert!(s.tooltip().is_none());
        }
    }
}
