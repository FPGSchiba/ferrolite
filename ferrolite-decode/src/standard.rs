//! Standard-raster decode route (JPEG/PNG/TIFF/WebP/BMP/GIF) via the `image`
//! crate, with EXIF read through `kamadak-exif`. Mirrors the RAW route's
//! products so everything downstream stays format-agnostic.

use crate::error::DecodeError;
use crate::metadata::Metadata;
use crate::orient::apply_orientation;
use ferrolite_image::{ImageBuffer, Orientation, PixelFormat};
use std::path::Path;

fn read_exif(path: &Path) -> Option<exif::Exif> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = std::io::BufReader::new(file);
    exif::Reader::new().read_from_container(&mut buf).ok()
}

/// Trim EXIF ASCII padding (trailing NULs and surrounding whitespace).
fn clean_ascii(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .trim()
        .to_string()
}

fn ascii(e: &exif::Exif, tag: exif::Tag) -> Option<String> {
    e.get_field(tag, exif::In::PRIMARY)
        .and_then(|f| match &f.value {
            exif::Value::Ascii(v) => v.first().map(|s| clean_ascii(s)),
            _ => None,
        })
}

fn uint(e: &exif::Exif, tag: exif::Tag) -> Option<u32> {
    e.get_field(tag, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
}

fn rational_f32(e: &exif::Exif, tag: exif::Tag) -> Option<f32> {
    e.get_field(tag, exif::In::PRIMARY)
        .and_then(|f| match &f.value {
            exif::Value::Rational(v) => v.first().map(|r| r.to_f32()),
            _ => None,
        })
}

fn orientation_of(e: &exif::Exif) -> Orientation {
    uint(e, exif::Tag::Orientation)
        .map(|v| Orientation::from_exif(v as u16))
        .unwrap_or(Orientation::Normal)
}

/// Read dimensions (cheap header read) + any present EXIF for a standard raster.
pub fn read_metadata_standard(path: &Path) -> Result<Metadata, DecodeError> {
    let (width, height) = image::image_dimensions(path)?;
    let exif = read_exif(path);
    let (make, model, orientation, iso, aperture, shutter, focal_length, capture_time, lens) =
        match exif.as_ref() {
            Some(e) => (
                ascii(e, exif::Tag::Make).unwrap_or_default(),
                ascii(e, exif::Tag::Model).unwrap_or_default(),
                orientation_of(e),
                uint(e, exif::Tag::PhotographicSensitivity),
                rational_f32(e, exif::Tag::FNumber),
                rational_f32(e, exif::Tag::ExposureTime),
                rational_f32(e, exif::Tag::FocalLength),
                ascii(e, exif::Tag::DateTimeOriginal),
                ascii(e, exif::Tag::LensModel),
            ),
            None => (
                String::new(),
                String::new(),
                Orientation::Normal,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
        };
    Ok(Metadata {
        make,
        model,
        width,
        height,
        orientation,
        iso,
        aperture,
        shutter,
        focal_length,
        capture_time,
        lens,
    })
}

/// Decode an upright RGB8 preview from a standard raster (orientation applied).
pub fn decode_preview_standard(path: &Path) -> Result<ImageBuffer, DecodeError> {
    let dynimg = image::open(path)?;
    let orientation = read_exif(path)
        .as_ref()
        .map(orientation_of)
        .unwrap_or(Orientation::Normal);
    let oriented = apply_orientation(dynimg, orientation);
    let rgb = oriented.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    Ok(ImageBuffer::new(w, h, PixelFormat::Rgb8, rgb.into_raw())
        .expect("RGB8 buffer length is w*h*3 by construction"))
}

#[cfg(test)]
mod tests {
    use super::clean_ascii;

    #[test]
    fn clean_ascii_strips_trailing_nul() {
        assert_eq!(clean_ascii(b"Canon\0"), "Canon");
    }

    #[test]
    fn clean_ascii_strips_trailing_space() {
        assert_eq!(clean_ascii(b"Canon "), "Canon");
    }

    #[test]
    fn clean_ascii_preserves_internal_spaces() {
        assert_eq!(clean_ascii(b"NIKON Z 6"), "NIKON Z 6");
    }
}
