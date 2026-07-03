//! Standard-raster decode route (JPEG/PNG/TIFF/WebP/BMP/GIF) via the `image`
//! crate, with EXIF read through `kamadak-exif`. Mirrors the RAW route's
//! products so everything downstream stays format-agnostic.

use crate::error::DecodeError;
use crate::metadata::Metadata;
use crate::orient::apply_orientation;
use ferrolite_image::{ImageBuffer, Orientation, PixelFormat};
use image::{DynamicImage, RgbImage};
use std::io::BufReader;
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

/// A downscaled preview for the *thumbnail* path only. JPEGs are DCT-scaled at
/// decode time (huge speedup vs a full 24 MP decode); other rasters fall back to
/// a full decode. NEVER used by the viewer/export full-res path.
#[derive(Debug, Clone)]
pub struct StdThumbDecode {
    pub image: ImageBuffer,
    pub dct_scale: u8,
    pub decoded_w: u32,
    pub decoded_h: u32,
}

/// True if `path` is a JPEG by extension AND magic bytes (`FF D8 FF`).
fn is_jpeg(path: &Path) -> bool {
    let ext_ok = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"))
        .unwrap_or(false);
    if !ext_ok {
        return false;
    }
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut magic = [0u8; 3];
    use std::io::Read;
    matches!(f.read_exact(&mut magic), Ok(())) && magic == [0xFF, 0xD8, 0xFF]
}

/// Decode a size-targeted preview for the thumbnail path. `target_edge` is the
/// thumbnail max edge (caller passes `THUMB_MAX_EDGE`); the JPEG path decodes at
/// the smallest DCT factor whose output stays >= `target_edge` on at least one
/// axis, so downstream resize has enough pixels but the IDCT does ~scale^2 less
/// work. EXIF orientation is applied (matching `decode_preview_standard`).
pub fn decode_thumb_source_standard(
    path: &Path,
    target_edge: u32,
    _measure: bool,
) -> Result<StdThumbDecode, DecodeError> {
    if is_jpeg(path) {
        decode_thumb_jpeg(path, target_edge)
    } else {
        // Non-JPEG: full decode + orient (same body as decode_preview_standard).
        decode_thumb_source_standard_fallback(path)
    }
}

fn decode_thumb_jpeg(path: &Path, target_edge: u32) -> Result<StdThumbDecode, DecodeError> {
    use jpeg_decoder::{Decoder, PixelFormat as JpegFmt};

    let file = std::fs::File::open(path)?;
    let mut dec = Decoder::new(BufReader::new(file));
    dec.read_info()
        .map_err(|e| DecodeError::Jpeg(format!("jpeg read_info: {e}")))?;
    let full = dec
        .info()
        .ok_or_else(|| DecodeError::Jpeg("jpeg info missing".into()))?;
    let full_w = full.width as u32;

    // Ask for target on both axes; jpeg-decoder picks the smallest supported DCT
    // factor (1/8,1/4,1/2,1) yielding >= requested on at least one axis.
    // Clamp is intentional and safe: jpeg-decoder's `scale` takes u16, and no
    // realistic thumbnail edge size approaches u16::MAX.
    let te = target_edge.min(u16::MAX as u32) as u16;
    let (sw, sh) = dec
        .scale(te, te)
        .map_err(|e| DecodeError::Jpeg(format!("jpeg scale: {e}")))?;
    let pixels = dec
        .decode()
        .map_err(|e| DecodeError::Jpeg(format!("jpeg decode: {e}")))?;
    let info = dec
        .info()
        .ok_or_else(|| DecodeError::Jpeg("jpeg info missing".into()))?;
    let (dw, dh) = (info.width as u32, info.height as u32);
    debug_assert_eq!((sw as u32, sh as u32), (dw, dh));

    // Normalize to RGB8. L8/RGB24 handled directly; L16/CMYK32 (rare) fall back
    // to a full image-crate decode for correctness.
    let rgb: Vec<u8> = match info.pixel_format {
        JpegFmt::RGB24 => pixels,
        JpegFmt::L8 => {
            let mut out = Vec::with_capacity(pixels.len() * 3);
            for g in pixels {
                out.extend_from_slice(&[g, g, g]);
            }
            out
        }
        _ => return decode_thumb_source_standard_fallback(path),
    };

    // Reuse the shared orientation logic via a DynamicImage.
    let orientation = read_exif(path)
        .as_ref()
        .map(orientation_of)
        .unwrap_or(Orientation::Normal);
    let rgbimg = RgbImage::from_raw(dw, dh, rgb)
        .ok_or_else(|| DecodeError::Jpeg("jpeg buffer length mismatch".into()))?;
    let oriented = apply_orientation(DynamicImage::ImageRgb8(rgbimg), orientation).to_rgb8();
    let (ow, oh) = (oriented.width(), oriented.height());
    let image = ImageBuffer::new(ow, oh, PixelFormat::Rgb8, oriented.into_raw())
        .expect("RGB8 buffer length is w*h*3 by construction");

    let dct_scale = (full_w / dw.max(1)).clamp(1, 8) as u8;
    Ok(StdThumbDecode {
        image,
        dct_scale,
        decoded_w: dw,
        decoded_h: dh,
    })
}

/// Full-decode fallback wrapped as a StdThumbDecode (dct_scale = 1).
fn decode_thumb_source_standard_fallback(path: &Path) -> Result<StdThumbDecode, DecodeError> {
    let image = decode_preview_standard(path)?;
    let (w, h) = (image.width, image.height);
    Ok(StdThumbDecode {
        image,
        dct_scale: 1,
        decoded_w: w,
        decoded_h: h,
    })
}

#[cfg(test)]
mod tests {
    use super::clean_ascii;
    use super::{decode_preview_standard, decode_thumb_source_standard};
    use ferrolite_image::PixelFormat;
    use image::{ImageBuffer as ImgBuf, Rgb};

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

    /// Encode a solid-ish RGB image to a temp .jpg and return its path.
    fn temp_jpeg(name: &str, w: u32, h: u32) -> std::path::PathBuf {
        let mut img = ImgBuf::<Rgb<u8>, _>::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
        }
        let mut p = std::env::temp_dir();
        p.push(format!("ferrolite-std-test-{name}.jpg"));
        img.save(&p).unwrap();
        p
    }

    #[test]
    fn thumb_decode_downscales_large_jpeg_via_dct() {
        let path = temp_jpeg("large", 4000, 3000);
        let out = decode_thumb_source_standard(&path, 256, false).unwrap();
        // 4000/256 -> smallest DCT factor keeping >=256 on an axis is 1/8 (=500x375).
        assert_eq!(
            out.dct_scale, 8,
            "expected 1/8 DCT scale for a 4000px source"
        );
        assert!(out.decoded_w >= 256 || out.decoded_h >= 256);
        // decoded well under source: pixels reduced ~64x.
        assert!(out.decoded_w <= 600 && out.decoded_h <= 600);
        assert_eq!(out.image.format, PixelFormat::Rgb8);
        assert_eq!(out.image.width, out.decoded_w);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn thumb_decode_scale_one_matches_full_decode_dims() {
        // target larger than the image -> no DCT reduction (scale 1), same dims as full.
        let path = temp_jpeg("small", 200, 150);
        let thumb = decode_thumb_source_standard(&path, 256, false).unwrap();
        let full = decode_preview_standard(&path).unwrap();
        assert_eq!(thumb.dct_scale, 1);
        assert_eq!(
            (thumb.image.width, thumb.image.height),
            (full.width, full.height)
        );
        // `decode_preview_standard` decodes JPEG via the `image` crate (zune-jpeg
        // internally); the thumb path decodes the same bytes via `jpeg-decoder`.
        // Two independent JPEG codecs are not guaranteed bit-identical even at
        // DCT scale 1 (IDCT/YCbCr->RGB rounding differs by a few LSBs) so we
        // assert near-equality rather than exact equality.
        assert_eq!(thumb.image.pixels.len(), full.pixels.len());
        let max_diff = thumb
            .image
            .pixels
            .iter()
            .zip(full.pixels.iter())
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap_or(0);
        assert!(
            max_diff <= 3,
            "scale-1 pixels should closely match full decode (max diff {max_diff})"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn thumb_decode_falls_back_for_png() {
        // PNG has no DCT path -> full decode + resize, scale reported as 1.
        let mut img = ImgBuf::<Rgb<u8>, _>::new(300, 200);
        for px in img.pixels_mut() {
            *px = Rgb([10, 20, 30]);
        }
        let mut p = std::env::temp_dir();
        p.push("ferrolite-std-test-fallback.png");
        img.save(&p).unwrap();
        let out = decode_thumb_source_standard(&p, 256, false).unwrap();
        assert_eq!(out.dct_scale, 1);
        assert_eq!((out.image.width, out.image.height), (300, 200));
        assert_eq!(out.image.format, PixelFormat::Rgb8);
        let _ = std::fs::remove_file(&p);
    }
}
