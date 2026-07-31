//! Export options (spec §8.2). Shared by the single flow (Plan 4) and the batch
//! Export module (Plan 5). `Default` encodes the spec defaults.

use ferrolite_color::WorkingSpace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Jpeg,
    Png,
    Tiff,
    WebP,
    Avif,
    JpegXl,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 6] = [
        ExportFormat::Jpeg,
        ExportFormat::Png,
        ExportFormat::Tiff,
        ExportFormat::WebP,
        ExportFormat::Avif,
        ExportFormat::JpegXl,
    ];

    /// Lower-case file extension (no dot).
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Jpeg => "jpg",
            ExportFormat::Png => "png",
            ExportFormat::Tiff => "tif",
            ExportFormat::WebP => "webp",
            ExportFormat::Avif => "avif",
            ExportFormat::JpegXl => "jxl",
        }
    }

    /// Human label for the format combo.
    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Jpeg => "JPEG",
            ExportFormat::Png => "PNG",
            ExportFormat::Tiff => "TIFF",
            ExportFormat::WebP => "WebP (lossless)",
            ExportFormat::Avif => "AVIF",
            ExportFormat::JpegXl => "JPEG-XL (lossless)",
        }
    }

    /// 16-bit output is supported for TIFF, PNG, and JPEG-XL.
    pub fn supports_16bit(self) -> bool {
        matches!(
            self,
            ExportFormat::Tiff | ExportFormat::Png | ExportFormat::JpegXl
        )
    }

    /// JPEG and AVIF are lossy and honor the quality setting.
    pub fn supports_quality(self) -> bool {
        matches!(self, ExportFormat::Jpeg | ExportFormat::Avif)
    }

    /// Only AVIF exposes the Effort (speed) control.
    pub fn supports_effort(self) -> bool {
        matches!(self, ExportFormat::Avif)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    Eight,
    Sixteen,
}

/// AVIF encode effort: speed-vs-size/quality tradeoff. Maps to
/// ravif's speed (1 = slow/best … 10 = fast/worst). `Best` is deliberately 3,
/// not 1 ("very very slow"), to avoid pathological export times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Fast,
    Balanced,
    Best,
}

impl Effort {
    pub fn ravif_speed(self) -> u8 {
        match self {
            Effort::Fast => 10,
            Effort::Balanced => 6,
            Effort::Best => 3,
        }
    }
}

/// Optional output resize (spec §8.1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResizeSpec {
    None,
    /// Scale so the longer edge equals this many pixels (aspect preserved).
    LongEdge(u32),
    /// Exact width×height (aspect may change).
    Exact {
        w: u32,
        h: u32,
    },
    /// Scale both axes by this fraction (1.0 = unchanged).
    Percent(f32),
}

/// Output medium for export sharpening (design §5.1). Selects the unsharp
/// radius: `Screen` crispest, `Matte` widest to fight paper dot gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputMedium {
    #[default]
    None,
    Screen,
    Glossy,
    Matte,
}

/// Output-sharpening strength tier. Scales the medium's amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputSharpenAmount {
    Low,
    #[default]
    Standard,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub output_space: WorkingSpace,
    pub bit_depth: BitDepth,
    /// JPEG (and WebP if it were lossy) quality 1..=100. Ignored otherwise.
    pub quality: u8,
    /// AVIF encode effort. Ignored for other formats.
    pub effort: Effort,
    pub resize: ResizeSpec,
    pub copy_exif: bool,
    pub embed_icc: bool,
    pub strip_metadata: bool,
    /// Output medium for export sharpening. `None` = no output sharpening.
    pub sharpen_for: OutputMedium,
    /// Strength tier for output sharpening. Ignored when `sharpen_for` is `None`.
    pub sharpen_amount: OutputSharpenAmount,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ExportFormat::Jpeg,
            output_space: WorkingSpace::Srgb, // web-safe default (§8.2)
            bit_depth: BitDepth::Eight,
            quality: 90,
            effort: Effort::Balanced,
            resize: ResizeSpec::None,
            copy_exif: true,
            embed_icc: true,
            strip_metadata: false,
            sharpen_for: OutputMedium::None,
            sharpen_amount: OutputSharpenAmount::Standard,
        }
    }
}

impl ExportOptions {
    /// The bit depth actually used: `Sixteen` only when the format supports it,
    /// else `Eight` (spec §8.2).
    pub fn effective_bit_depth(&self) -> BitDepth {
        match self.bit_depth {
            BitDepth::Sixteen if self.format.supports_16bit() => BitDepth::Sixteen,
            _ => BitDepth::Eight,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec_8_2() {
        let o = ExportOptions::default();
        assert_eq!(o.format, ExportFormat::Jpeg);
        assert_eq!(o.output_space, ferrolite_color::WorkingSpace::Srgb);
        assert_eq!(o.bit_depth, BitDepth::Eight);
        assert_eq!(o.quality, 90);
        assert_eq!(o.resize, ResizeSpec::None);
        assert!(o.copy_exif);
        assert!(o.embed_icc);
        assert!(!o.strip_metadata);
    }

    #[test]
    fn sixteen_bit_only_for_tiff_and_png() {
        for f in ExportFormat::ALL {
            let o = ExportOptions {
                format: f,
                bit_depth: BitDepth::Sixteen,
                ..Default::default()
            };
            let expected = if f.supports_16bit() {
                BitDepth::Sixteen
            } else {
                BitDepth::Eight
            };
            assert_eq!(o.effective_bit_depth(), expected, "{f:?}");
        }
        assert!(ExportFormat::Tiff.supports_16bit());
        assert!(ExportFormat::Png.supports_16bit());
        assert!(!ExportFormat::Jpeg.supports_16bit());
        assert!(!ExportFormat::WebP.supports_16bit());
    }

    #[test]
    fn only_jpeg_uses_quality() {
        assert!(ExportFormat::Jpeg.supports_quality());
        assert!(!ExportFormat::Png.supports_quality());
        assert!(!ExportFormat::Tiff.supports_quality());
        assert!(!ExportFormat::WebP.supports_quality());
    }

    #[test]
    fn six_formats_with_stable_extensions_and_labels() {
        assert_eq!(ExportFormat::ALL.len(), 6);
        assert_eq!(ExportFormat::Avif.extension(), "avif");
        assert_eq!(ExportFormat::JpegXl.extension(), "jxl");
        assert_eq!(ExportFormat::Avif.label(), "AVIF");
        assert_eq!(ExportFormat::JpegXl.label(), "JPEG-XL (lossless)");
        // AVIF is lossy → honors quality; JXL is lossless → does not.
        assert!(ExportFormat::Avif.supports_quality());
        assert!(!ExportFormat::JpegXl.supports_quality());
        // JXL joins TIFF/PNG for 16-bit; AVIF is 8-bit only.
        assert!(ExportFormat::JpegXl.supports_16bit());
        assert!(!ExportFormat::Avif.supports_16bit());
    }

    #[test]
    fn extensions_are_stable() {
        assert_eq!(ExportFormat::Jpeg.extension(), "jpg");
        assert_eq!(ExportFormat::Png.extension(), "png");
        assert_eq!(ExportFormat::Tiff.extension(), "tif");
        assert_eq!(ExportFormat::WebP.extension(), "webp");
    }

    #[test]
    fn effort_maps_to_ravif_speed() {
        assert_eq!(Effort::Fast.ravif_speed(), 10);
        assert_eq!(Effort::Balanced.ravif_speed(), 6);
        assert_eq!(Effort::Best.ravif_speed(), 3);
    }

    #[test]
    fn only_avif_supports_effort_and_default_is_balanced() {
        for f in ExportFormat::ALL {
            assert_eq!(
                f.supports_effort(),
                matches!(f, ExportFormat::Avif),
                "{f:?}"
            );
        }
        assert_eq!(ExportOptions::default().effort, Effort::Balanced);
    }
}
