use crate::error::{rawler as rawler_err, DecodeError};
use crate::orient::apply_orientation;
use ferrolite_image::{ImageBuffer, Orientation, PixelFormat};
use rawler::decoders::{Decoder, RawDecodeParams};
use rawler::rawsource::RawSource;
use std::path::Path;

/// Extract an upright RGB8 preview using an already-constructed decoder and the
/// EXIF orientation already read from its metadata. Shared by `decode_preview_raw`
/// and the single-pass `decode_meta_and_preview` so the file is parsed once.
pub(crate) fn preview_from_decoder(
    decoder: &dyn Decoder,
    src: &RawSource,
    exif_orientation: u16,
) -> Result<ImageBuffer, DecodeError> {
    let params = RawDecodeParams::default();
    let dynimg = decoder
        .preview_image(src, &params)
        .ok()
        .flatten()
        .or_else(|| decoder.full_image(src, &params).ok().flatten())
        .or_else(|| decoder.thumbnail_image(src, &params).ok().flatten())
        .ok_or(DecodeError::NoPreview(std::path::PathBuf::new()))?;
    let oriented = apply_orientation(dynimg, Orientation::from_exif(exif_orientation));
    let rgb = oriented.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    Ok(ImageBuffer::new(w, h, PixelFormat::Rgb8, rgb.into_raw())
        .expect("RGB8 buffer length is w*h*3 by construction"))
}

/// Decode an upright RGB8 preview from a RAW's embedded JPEG (see module note).
/// Uses a sequential prefix read (not mmap page-faults) so slow disks aren't
/// seek-thrashed — see `source::with_ingest_source`.
pub fn decode_preview_raw(path: &Path) -> Result<ImageBuffer, DecodeError> {
    crate::source::with_ingest_source(path, |src| {
        let decoder = rawler::get_decoder(src).map_err(rawler_err)?;
        let params = RawDecodeParams::default();

        let exif_orientation = decoder
            .raw_metadata(src, &params)
            .map_err(rawler_err)?
            .exif
            .orientation
            .unwrap_or(1);

        preview_from_decoder(decoder.as_ref(), src, exif_orientation)
            .map_err(|_| DecodeError::NoPreview(path.to_path_buf()))
    })
}
