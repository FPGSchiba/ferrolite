//! RAW decode: the three independently-consumable products (preview, full,
//! metadata) the two-tier load path relies on. Wraps `rawler` 0.7.x.

mod error;
mod metadata;

pub use error::DecodeError;
pub use metadata::Metadata;

use ferrolite_image::Orientation;
use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;
use std::path::Path;

use crate::error::rawler as rawler_err;

/// rawler `Rational` → f32.
/// rawler 0.7.2 uses `n: u32` / `d: u32` (not `num`/`den`).
fn rat(n: u64, d: u64) -> Option<f32> {
    if d == 0 {
        None
    } else {
        Some(n as f32 / d as f32)
    }
}

/// Read camera/exposure metadata and image dimensions without decoding pixels.
/// Dimensions come from a `dummy` raw_image call (fills geometry, skips pixels).
pub fn read_metadata(path: &Path) -> Result<Metadata, DecodeError> {
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
        width: dims.width as u32,
        height: dims.height as u32,
        orientation: Orientation::from_exif(e.orientation.unwrap_or(1)),
        iso: e.iso_speed_ratings.map(u32::from),
        aperture: e.fnumber.as_ref().and_then(|r| rat(r.n as u64, r.d as u64)),
        shutter: e
            .exposure_time
            .as_ref()
            .and_then(|r| rat(r.n as u64, r.d as u64)),
        focal_length: e
            .focal_length
            .as_ref()
            .and_then(|r| rat(r.n as u64, r.d as u64)),
        capture_time: e.date_time_original.clone(),
        lens: e.lens_model.clone(),
    })
}
