use crate::error::{rawler as rawler_err, DecodeError};
use crate::orient::apply_orientation;
use ferrolite_image::{ImageBuffer, Orientation, PixelFormat};
use rawler::decoders::RawDecodeParams;
use std::path::Path;

/// Decode an upright RGB8 preview from a RAW's embedded JPEG (see module note).
/// Uses a sequential prefix read (not mmap page-faults) so slow disks aren't
/// seek-thrashed — see `source::with_ingest_source`.
pub fn decode_preview_raw(path: &Path) -> Result<ImageBuffer, DecodeError> {
    crate::source::with_ingest_source(path, |src| {
        let decoder = rawler::get_decoder(src).map_err(rawler_err)?;
        let params = RawDecodeParams::default();

        let dynimg = decoder
            .preview_image(src, &params)
            .ok()
            .flatten()
            .or_else(|| decoder.full_image(src, &params).ok().flatten())
            .or_else(|| decoder.thumbnail_image(src, &params).ok().flatten())
            .ok_or_else(|| DecodeError::NoPreview(path.to_path_buf()))?;

        let exif_orientation = decoder
            .raw_metadata(src, &params)
            .map_err(rawler_err)?
            .exif
            .orientation
            .unwrap_or(1);
        let oriented = apply_orientation(dynimg, Orientation::from_exif(exif_orientation));

        let rgb = oriented.to_rgb8();
        let (w, h) = (rgb.width(), rgb.height());
        Ok(ImageBuffer::new(w, h, PixelFormat::Rgb8, rgb.into_raw())
            .expect("RGB8 buffer length is w*h*3 by construction"))
    })
}
