//! Encode a `RenderedImage` to a file in the chosen format: JPEG/PNG/TIFF/WebP
//! with EXIF copy + embedded ICC, plus AVIF and JPEG-XL (written untagged, with
//! no embedded ICC). Best-effort ICC + never-panic per spec §10: a failed ICC
//! step downgrades to an untagged (but valid) file plus a warning.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::tiff::TiffEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder};
use zune_core::bit_depth::BitDepth as ZBitDepth;
use zune_core::colorspace::ColorSpace as ZColorSpace;
use zune_core::options::EncoderOptions as ZEncoderOptions;
use zune_jpegxl::JxlSimpleEncoder;

use crate::error::ExportError;
use crate::options::{ExportFormat, ExportOptions};
use crate::render::{PixelData, RenderedImage};

/// Open the destination as a buffered writer. Called lazily inside the four
/// `image`-crate arms so the AVIF/JXL arms (which write their own bytes via
/// `std::fs::write`) never create a stray empty file first.
fn open_writer(dest: &Path) -> Result<BufWriter<File>, ExportError> {
    let file = File::create(dest).map_err(|e| ExportError::Io(e.to_string()))?;
    Ok(BufWriter::new(file))
}

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
            let mut out = open_writer(dest)?;
            let mut enc = JpegEncoder::new_with_quality(&mut out, opts.quality);
            set_icc!(enc);
            enc.write_image(bytes, w, h, color)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::Png => {
            let mut out = open_writer(dest)?;
            let mut enc = PngEncoder::new(&mut out);
            set_icc!(enc);
            enc.write_image(bytes, w, h, color)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::Tiff => {
            // TiffEncoder::new is infallible in image 0.25.10 (no Result — the
            // brief assumed one, but the vendored source returns `TiffEncoder<W>`
            // directly). Needs Seek; BufWriter<File> is Seek.
            let mut out = open_writer(dest)?;
            let mut enc = TiffEncoder::new(&mut out);
            set_icc!(enc);
            enc.write_image(bytes, w, h, color)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::WebP => {
            // Lossless only (spec §2). Force 8-bit RGB.
            let mut out = open_writer(dest)?;
            let mut enc = WebPEncoder::new_lossless(&mut out);
            set_icc!(enc);
            enc.write_image(bytes, w, h, color)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::Avif => {
            // ravif encodes 8-bit RGB. If a 16-bit buffer reaches here, down-shift
            // to 8-bit (effective_bit_depth() forces 8-bit for AVIF, so this is
            // defensive only).
            let rgb8: Vec<u8> = match &img.data {
                PixelData::Eight(v) => v.clone(),
                PixelData::Sixteen(v) => v.iter().map(|&s| (s >> 8) as u8).collect(),
            };
            let pixels: Vec<ravif::RGB8> = rgb8
                .chunks_exact(3)
                .map(|c| ravif::RGB8::new(c[0], c[1], c[2]))
                .collect();
            let encoded = ravif::Encoder::new()
                .with_quality(opts.quality as f32)
                .with_speed(opts.effort.ravif_speed())
                .encode_rgb(ravif::Img::new(pixels.as_slice(), w as usize, h as usize))
                .map_err(|e| ExportError::Encode(e.to_string()))?;
            std::fs::write(dest, &encoded.avif_file).map_err(|e| ExportError::Io(e.to_string()))?;
            if icc.is_some() {
                warnings.push("ICC embedding not supported for AVIF; file is untagged".to_string());
            }
        }
        ExportFormat::JpegXl => {
            let (zdepth, data): (ZBitDepth, Vec<u8>) = match &img.data {
                PixelData::Eight(v) => (ZBitDepth::Eight, v.clone()),
                PixelData::Sixteen(v) => (
                    ZBitDepth::Sixteen,
                    bytemuck::cast_slice::<u16, u8>(v).to_vec(),
                ),
            };
            let zopts = ZEncoderOptions::new(w as usize, h as usize, ZColorSpace::RGB, zdepth);
            let encoded = JxlSimpleEncoder::new(&data, zopts)
                .encode()
                .map_err(|e| ExportError::Encode(format!("{e:?}")))?;
            std::fs::write(dest, &encoded).map_err(|e| ExportError::Io(e.to_string()))?;
            if icc.is_some() {
                warnings
                    .push("ICC embedding not supported for JPEG-XL; file is untagged".to_string());
            }
        }
    }

    Ok(warnings)
}
