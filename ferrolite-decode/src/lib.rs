//! RAW decode: the three independently-consumable products (preview, full,
//! metadata) the two-tier load path relies on. Wraps `rawler` 0.7.x.

mod error;
mod metadata;
mod orient;
mod preview;
mod raw;
mod standard;

pub use error::DecodeError;
pub use metadata::Metadata;
pub use raw::{decode_full, RawDecoded};
pub use standard::{decode_preview_standard, read_metadata_standard};

use ferrolite_image::{FileKind, ImageBuffer, Orientation};
use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;
use std::path::Path;

use crate::error::rawler as rawler_err;

/// Decode an upright RGB8 preview, routed by `kind`.
pub fn decode_preview(path: &Path, kind: FileKind) -> Result<ImageBuffer, DecodeError> {
    match kind {
        FileKind::Raw => preview::decode_preview_raw(path),
        FileKind::Standard => standard::decode_preview_standard(path),
    }
}

/// Read camera/exposure metadata + dimensions, routed by `kind`.
pub fn read_metadata(path: &Path, kind: FileKind) -> Result<Metadata, DecodeError> {
    match kind {
        FileKind::Raw => read_metadata_raw(path),
        FileKind::Standard => standard::read_metadata_standard(path),
    }
}

/// rawler `Rational` → f32.
/// rawler 0.7.2 uses `n: u32` / `d: u32` (not `num`/`den`).
fn rat(n: u32, d: u32) -> Option<f32> {
    if d == 0 {
        None
    } else {
        Some(n as f32 / d as f32)
    }
}

/// RAW metadata via rawler (dimensions from a `dummy` decode; no pixel work).
fn read_metadata_raw(path: &Path) -> Result<Metadata, DecodeError> {
    let src = RawSource::new(path).map_err(rawler_err)?;
    let decoder = rawler::get_decoder(&src).map_err(rawler_err)?;
    let params = RawDecodeParams::default();

    let meta = decoder.raw_metadata(&src, &params).map_err(rawler_err)?;
    // `dummy = true`: geometry only, no pixel decode (fast).
    let dims = decoder.raw_image(&src, &params, true).map_err(rawler_err)?;

    let e = &meta.exif;
    Ok(Metadata {
        make: meta.make.clone(),
        model: meta.model.clone(),
        width: u32::try_from(dims.width)
            .map_err(|_| DecodeError::Rawler("RAW width exceeds u32".into()))?,
        height: u32::try_from(dims.height)
            .map_err(|_| DecodeError::Rawler("RAW height exceeds u32".into()))?,
        orientation: Orientation::from_exif(e.orientation.unwrap_or(1)),
        iso: e.iso_speed_ratings.map(u32::from),
        aperture: e.fnumber.as_ref().and_then(|r| rat(r.n, r.d)),
        shutter: e.exposure_time.as_ref().and_then(|r| rat(r.n, r.d)),
        focal_length: e.focal_length.as_ref().and_then(|r| rat(r.n, r.d)),
        capture_time: e.date_time_original.clone(),
        lens: e.lens_model.clone(),
    })
}
