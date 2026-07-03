use crate::error::{rawler as rawler_err, DecodeError};
use crate::orient::apply_orientation;
use ferrolite_image::{ImageBuffer, Orientation, PixelFormat};
use rawler::decoders::{Decoder, RawDecodeParams};
use rawler::rawsource::RawSource;
use std::path::Path;
use std::time::{Duration, Instant};

/// Which embedded image the RAW preview path used. In rawler 0.7.2 no decoder
/// implements `preview_image`, so RAW previews come from `full_image` (the
/// full-resolution embedded JPEG) or, rarely, `thumbnail_image`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSource {
    EmbeddedPreview,
    FullImage,
    EmbeddedThumbnail,
}

/// What the preview extraction did, for diagnostics. `source`/`src_w`/`src_h`
/// are always populated (free); `extract`/`orient` are `Some` only when the
/// caller passes `measure = true` (zero `Instant` cost when false).
#[derive(Debug, Clone, Copy)]
pub struct PreviewInfo {
    pub source: PreviewSource,
    pub src_w: u32,
    pub src_h: u32,
    pub extract: Option<Duration>,
    pub orient: Option<Duration>,
}

/// Extract an upright RGB8 preview using an already-constructed decoder and the
/// EXIF orientation already read from its metadata. Shared by `decode_preview_raw`
/// and the single-pass `decode_meta_and_preview` so the file is parsed once.
/// When `measure` is true, times the embedded-image decode (`extract`) separately
/// from the orientation + RGB8 conversion (`orient`).
pub(crate) fn preview_from_decoder(
    decoder: &dyn Decoder,
    src: &RawSource,
    exif_orientation: u16,
    measure: bool,
) -> Result<(ImageBuffer, PreviewInfo), DecodeError> {
    let params = RawDecodeParams::default();

    let t_extract = measure.then(Instant::now);
    let (dynimg, source) = if let Some(img) = decoder.preview_image(src, &params).ok().flatten() {
        (img, PreviewSource::EmbeddedPreview)
    } else if let Some(img) = decoder.full_image(src, &params).ok().flatten() {
        (img, PreviewSource::FullImage)
    } else if let Some(img) = decoder.thumbnail_image(src, &params).ok().flatten() {
        (img, PreviewSource::EmbeddedThumbnail)
    } else {
        return Err(DecodeError::NoPreview(std::path::PathBuf::new()));
    };
    let extract = t_extract.map(|t| t.elapsed());
    let (src_w, src_h) = (dynimg.width(), dynimg.height());

    let t_orient = measure.then(Instant::now);
    let oriented = apply_orientation(dynimg, Orientation::from_exif(exif_orientation));
    let rgb = oriented.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    let buf = ImageBuffer::new(w, h, PixelFormat::Rgb8, rgb.into_raw())
        .expect("RGB8 buffer length is w*h*3 by construction");
    let orient = t_orient.map(|t| t.elapsed());

    Ok((
        buf,
        PreviewInfo {
            source,
            src_w,
            src_h,
            extract,
            orient,
        },
    ))
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

        preview_from_decoder(decoder.as_ref(), src, exif_orientation, false)
            .map(|(buf, _info)| buf)
            .map_err(|_| DecodeError::NoPreview(path.to_path_buf()))
    })
}
