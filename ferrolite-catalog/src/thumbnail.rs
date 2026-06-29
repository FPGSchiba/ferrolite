use crate::catalog::Catalog;
use crate::error::CatalogError;
use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use ferrolite_image::{ImageBuffer, PixelFormat};
use image::codecs::jpeg::JpegEncoder;
use image::ExtendedColorType;

pub const THUMB_MAX_EDGE: u32 = 256;
pub const THUMB_QUALITY: u8 = 85;
const THUMB_LEVEL: i64 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub bytes: Vec<u8>,
}

/// Storage for thumbnail blobs. A trait so a memory-mapped mipmap cache can
/// replace the SQLite-BLOB impl later with zero call-site change (design §4).
pub trait ThumbnailStore {
    fn put_thumbnail(&self, image_id: i64, thumb: &Thumbnail) -> Result<(), CatalogError>;
    fn get_thumbnail(&self, image_id: i64) -> Result<Option<Thumbnail>, CatalogError>;
}

/// Resize an RGB8 preview to fit within `THUMB_MAX_EDGE` (aspect preserved,
/// never upscaled) and encode it as JPEG q85.
pub fn generate_thumbnail(preview: &ImageBuffer) -> Result<Thumbnail, CatalogError> {
    // JPEG has no alpha; drop it if the source is RGBA.
    let (rgb, src_w, src_h) = to_rgb8(preview);

    let scale = (THUMB_MAX_EDGE as f32 / src_w as f32)
        .min(THUMB_MAX_EDGE as f32 / src_h as f32)
        .min(1.0);
    let dst_w = ((src_w as f32 * scale).round() as u32).max(1);
    let dst_h = ((src_h as f32 * scale).round() as u32).max(1);

    let src_img = Image::from_vec_u8(src_w, src_h, rgb, PixelType::U8x3)
        .map_err(|e| CatalogError::Encode(e.to_string()))?;
    let mut dst_img = Image::new(dst_w, dst_h, PixelType::U8x3);
    let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    Resizer::new()
        .resize(&src_img, &mut dst_img, &opts)
        .map_err(|e| CatalogError::Encode(e.to_string()))?;

    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, THUMB_QUALITY)
        .encode(dst_img.buffer(), dst_w, dst_h, ExtendedColorType::Rgb8)
        .map_err(|e| CatalogError::Encode(e.to_string()))?;

    Ok(Thumbnail {
        width: dst_w,
        height: dst_h,
        format: "jpeg".to_string(),
        bytes,
    })
}

/// Return tightly-packed RGB8 bytes plus dimensions, dropping alpha if present.
fn to_rgb8(buf: &ImageBuffer) -> (Vec<u8>, u32, u32) {
    match buf.format {
        PixelFormat::Rgb8 => (buf.pixels.clone(), buf.width, buf.height),
        PixelFormat::Rgba8 => {
            let mut rgb = Vec::with_capacity(buf.pixels.len() / 4 * 3);
            for px in buf.pixels.chunks_exact(4) {
                rgb.extend_from_slice(&px[0..3]);
            }
            (rgb, buf.width, buf.height)
        }
    }
}

impl ThumbnailStore for Catalog {
    fn put_thumbnail(&self, image_id: i64, thumb: &Thumbnail) -> Result<(), CatalogError> {
        self.conn().execute(
            "INSERT INTO thumbnails (image_id, level, w, h, format, blob)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(image_id) DO UPDATE SET
                level=?2, w=?3, h=?4, format=?5, blob=?6",
            rusqlite::params![
                image_id,
                THUMB_LEVEL,
                thumb.width as i64,
                thumb.height as i64,
                thumb.format,
                thumb.bytes,
            ],
        )?;
        Ok(())
    }

    fn get_thumbnail(&self, image_id: i64) -> Result<Option<Thumbnail>, CatalogError> {
        crate::queries::get_thumbnail(self.conn(), image_id)
    }
}
