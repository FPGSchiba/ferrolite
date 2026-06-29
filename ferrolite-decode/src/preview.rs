use crate::error::{rawler as rawler_err, DecodeError};
use ferrolite_image::{ImageBuffer, Orientation, PixelFormat};
use image::DynamicImage;
use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;
use std::path::Path;

/// Decode an upright RGB8 preview. Tries the embedded preview JPEG, then the
/// embedded full-size JPEG, then the embedded thumbnail — first one present
/// wins. Orientation from EXIF is applied so the result is display-upright.
pub fn decode_preview(path: &Path) -> Result<ImageBuffer, DecodeError> {
    let src = RawSource::new(path).map_err(rawler_err)?;
    let decoder = rawler::get_decoder(&src).map_err(rawler_err)?;
    let params = RawDecodeParams::default();

    let dynimg = decoder
        .preview_image(&src, &params)
        .ok()
        .flatten()
        .or_else(|| decoder.full_image(&src, &params).ok().flatten())
        .or_else(|| decoder.thumbnail_image(&src, &params).ok().flatten())
        .ok_or_else(|| DecodeError::NoPreview(path.to_path_buf()))?;

    let exif_orientation = decoder
        .raw_metadata(&src, &params)
        .map_err(rawler_err)?
        .exif
        .orientation
        .unwrap_or(1);
    let oriented = apply_orientation(dynimg, Orientation::from_exif(exif_orientation));

    let rgb = oriented.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    Ok(ImageBuffer::new(w, h, PixelFormat::Rgb8, rgb.into_raw())
        .expect("RGB8 buffer length is w*h*3 by construction"))
}

/// Apply an EXIF orientation to a decoded image using the `image` crate's
/// rotate/flip ops. (rotate90/270 are clockwise in the `image` crate.)
fn apply_orientation(img: DynamicImage, o: Orientation) -> DynamicImage {
    match o {
        Orientation::Normal => img,
        Orientation::FlipH => img.fliph(),
        Orientation::Rotate180 => img.rotate180(),
        Orientation::FlipV => img.flipv(),
        Orientation::Transpose => img.rotate90().fliph(),
        Orientation::Rotate90 => img.rotate90(),
        Orientation::Transverse => img.rotate270().fliph(),
        Orientation::Rotate270 => img.rotate270(),
    }
}
