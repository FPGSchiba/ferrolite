//! Encode a `RenderedImage` to a file in the chosen format, embedding the output
//! ICC profile where the format supports it (via `ImageEncoder::set_icc_profile`).
//! Best-effort ICC + never-panic per spec §10: a failed ICC step downgrades to an
//! untagged (but valid) file plus a warning.

use std::io::BufWriter;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::tiff::TiffEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder};

use crate::error::ExportError;
use crate::options::{ExportFormat, ExportOptions};
use crate::render::{PixelData, RenderedImage};

pub(crate) fn encode_to_file(
    img: &RenderedImage,
    opts: &ExportOptions,
    dest: &Path,
) -> Result<Vec<String>, ExportError> {
    let mut warnings = Vec::new();

    // Emit the output ICC once (best-effort).
    let icc: Option<Vec<u8>> = if opts.embed_icc {
        match ferrolite_color::emit_icc(opts.output_space) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                warnings.push(format!("ICC profile not embedded (emit failed: {e})"));
                None
            }
        }
    } else {
        None
    };

    let (w, h) = (img.width, img.height);
    let (bytes, color): (&[u8], ExtendedColorType) = match &img.data {
        PixelData::Eight(v) => (v.as_slice(), ExtendedColorType::Rgb8),
        PixelData::Sixteen(v) => (bytemuck::cast_slice(v), ExtendedColorType::Rgb16),
    };

    let file = std::fs::File::create(dest).map_err(|e| ExportError::Io(e.to_string()))?;
    let mut out = BufWriter::new(file);

    // Each encoder: create, best-effort set ICC, then write_image (consumes self).
    macro_rules! set_icc {
        ($enc:expr) => {{
            if let Some(ref profile) = icc {
                if let Err(e) = $enc.set_icc_profile(profile.clone()) {
                    warnings.push(format!("ICC not embedded for this format: {e}"));
                }
            }
        }};
    }

    match opts.format {
        ExportFormat::Jpeg => {
            let mut enc = JpegEncoder::new_with_quality(&mut out, opts.quality);
            set_icc!(enc);
            enc.write_image(bytes, w, h, color)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::Png => {
            let mut enc = PngEncoder::new(&mut out);
            set_icc!(enc);
            enc.write_image(bytes, w, h, color)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::Tiff => {
            // TiffEncoder::new is infallible in image 0.25.10 (no Result — the
            // brief assumed one, but the vendored source returns `TiffEncoder<W>`
            // directly). Needs Seek; BufWriter<File> is Seek.
            let mut enc = TiffEncoder::new(&mut out);
            set_icc!(enc);
            enc.write_image(bytes, w, h, color)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::WebP => {
            // Lossless only (spec §2). Force 8-bit RGB.
            let mut enc = WebPEncoder::new_lossless(&mut out);
            set_icc!(enc);
            enc.write_image(bytes, w, h, color)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
    }

    Ok(warnings)
}
