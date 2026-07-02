//! Export options (spec §8.2). Shared by the single flow (Plan 4) and the batch
//! Export module (Plan 5). `Default` encodes the spec defaults.

use ferrolite_color::WorkingSpace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Jpeg,
    Png,
    Tiff,
    WebP,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 4] = [
        ExportFormat::Jpeg,
        ExportFormat::Png,
        ExportFormat::Tiff,
        ExportFormat::WebP,
    ];

    /// Lower-case file extension (no dot).
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Jpeg => "jpg",
            ExportFormat::Png => "png",
            ExportFormat::Tiff => "tif",
            ExportFormat::WebP => "webp",
        }
    }

    /// Human label for the format combo.
    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Jpeg => "JPEG",
            ExportFormat::Png => "PNG",
            ExportFormat::Tiff => "TIFF",
            ExportFormat::WebP => "WebP (lossless)",
        }
    }

    /// 16-bit output is supported only for TIFF and PNG (spec §8.2).
    pub fn supports_16bit(self) -> bool {
        matches!(self, ExportFormat::Tiff | ExportFormat::Png)
    }

    /// Only JPEG honors the quality setting (WebP is lossless; PNG/TIFF lossless).
    pub fn supports_quality(self) -> bool {
        matches!(self, ExportFormat::Jpeg)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    Eight,
    Sixteen,
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub output_space: WorkingSpace,
    pub bit_depth: BitDepth,
    /// JPEG (and WebP if it were lossy) quality 1..=100. Ignored otherwise.
    pub quality: u8,
    pub resize: ResizeSpec,
    pub copy_exif: bool,
    pub embed_icc: bool,
    pub strip_metadata: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ExportFormat::Jpeg,
            output_space: WorkingSpace::Srgb, // web-safe default (§8.2)
            bit_depth: BitDepth::Eight,
            quality: 90,
            resize: ResizeSpec::None,
            copy_exif: true,
            embed_icc: true,
            strip_metadata: false,
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
    fn extensions_are_stable() {
        assert_eq!(ExportFormat::Jpeg.extension(), "jpg");
        assert_eq!(ExportFormat::Png.extension(), "png");
        assert_eq!(ExportFormat::Tiff.extension(), "tif");
        assert_eq!(ExportFormat::WebP.extension(), "webp");
    }
}
