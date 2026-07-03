//! Unified decode entry point: routes preview and metadata requests by
//! `FileKind` — RAW files via `rawler` 0.7.x, standard rasters via `image` +
//! `kamadak-exif` — returning the same three separable products in both cases.

mod color;
mod demosaic;
mod error;
mod metadata;
mod orient;
mod preview;
mod raw;
mod source;
mod standard;

pub use color::ColorProfile;
pub use demosaic::{DemosaicParams, DemosaicToRgb16f, QuadBin};
pub use error::DecodeError;
pub use metadata::Metadata;
pub use orient::apply_orientation_linear;
pub use preview::PreviewSource;
pub use raw::{decode_full, RawDecoded};
pub use source::SourceKind;
pub use standard::{decode_preview_standard, read_metadata_standard};

use ferrolite_image::{FileKind, ImageBuffer, Orientation};
use rawler::decoders::{RawDecodeParams, RawMetadata};
use rawler::rawimage::RawImage;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::error::rawler as rawler_err;

/// Diagnostics for one `decode_meta_and_preview` call. `source`/`src_w`/`src_h`
/// are always populated; every `Option<Duration>` is `Some` only when the caller
/// passed `measure = true` (zero `Instant` cost otherwise). The timings split the
/// per-file cost into its stages: acquiring the byte source, `get_decoder`,
/// `raw_metadata`, the dummy-geometry `raw_image`, then the embedded-preview
/// `extract` and `orient`. `source_kind` reports whether the fast 1 MiB prefix
/// sufficed or the file fell back to the full mmap source; it is `None` for
/// standard (non-RAW) rasters, which do not use that machinery.
#[derive(Debug, Clone, Copy)]
pub struct PreviewInfo {
    pub source: PreviewSource,
    pub src_w: u32,
    pub src_h: u32,
    pub source_kind: Option<SourceKind>,
    pub source_acquire: Option<Duration>,
    /// Bytes read to satisfy the decode (`Some` only when measured).
    pub source_bytes: Option<u64>,
    pub get_decoder: Option<Duration>,
    pub raw_metadata: Option<Duration>,
    pub raw_dims: Option<Duration>,
    pub extract: Option<Duration>,
    pub orient: Option<Duration>,
}

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

/// Decode metadata AND an upright RGB8 preview in a SINGLE pass. For RAW this
/// runs ONE `get_decoder` + ONE `raw_metadata`, eliminating the double-open that
/// dominated ingest time (see investigation R2, RC-PERF-1).
pub fn decode_meta_and_preview(
    path: &Path,
    kind: FileKind,
    measure: bool,
) -> Result<(Metadata, ImageBuffer, PreviewInfo), DecodeError> {
    match kind {
        FileKind::Raw => {
            let (parts_bundle, probe) = crate::source::with_ingest_source(path, measure, |src| {
                let t = measure.then(Instant::now);
                let decoder = rawler::get_decoder(src).map_err(rawler_err)?;
                let get_decoder = t.map(|t| t.elapsed());

                let params = RawDecodeParams::default();

                let t = measure.then(Instant::now);
                let meta_raw = decoder.raw_metadata(src, &params).map_err(rawler_err)?;
                let raw_metadata = t.map(|t| t.elapsed());

                // `dummy = true`: geometry only, no pixel decode (fast on an in-memory source).
                let t = measure.then(Instant::now);
                let dims = decoder.raw_image(src, &params, true).map_err(rawler_err)?;
                let raw_dims = t.map(|t| t.elapsed());

                let exif_orientation = meta_raw.exif.orientation.unwrap_or(1);
                let metadata = build_metadata_from_raw(&meta_raw, &dims)?;
                let (preview, parts) = crate::preview::preview_from_decoder(
                    decoder.as_ref(),
                    src,
                    exif_orientation,
                    measure,
                )
                .map_err(|_| DecodeError::NoPreview(path.to_path_buf()))?;
                Ok((
                    metadata,
                    preview,
                    parts,
                    get_decoder,
                    raw_metadata,
                    raw_dims,
                ))
            })?;
            let (metadata, preview, parts, get_decoder, raw_metadata, raw_dims) = parts_bundle;
            let info = PreviewInfo {
                source: parts.source,
                src_w: parts.src_w,
                src_h: parts.src_h,
                source_kind: Some(probe.kind),
                source_acquire: probe.acquire,
                source_bytes: probe.bytes,
                get_decoder,
                raw_metadata,
                raw_dims,
                extract: parts.extract,
                orient: parts.orient,
            };
            Ok((metadata, preview, info))
        }
        FileKind::Standard => {
            let metadata = standard::read_metadata_standard(path)?;
            let preview = standard::decode_preview_standard(path)?;
            // Standard rasters are read directly (not through the prefix/mmap RAW
            // machinery) and are already fast; tag source as EmbeddedPreview,
            // `source_kind` None, and leave all sub-timings None.
            let info = PreviewInfo {
                source: PreviewSource::EmbeddedPreview,
                src_w: preview.width,
                src_h: preview.height,
                source_kind: None,
                source_acquire: None,
                source_bytes: None,
                get_decoder: None,
                raw_metadata: None,
                raw_dims: None,
                extract: None,
                orient: None,
            };
            Ok((metadata, preview, info))
        }
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

/// Build the shared `Metadata` from rawler's `RawMetadata` (camera/EXIF) and
/// `RawImage` (dimensions, from a `dummy` decode). Factored so the single-open
/// `read_metadata_raw` and the single-pass `decode_meta_and_preview` don't
/// duplicate the field mapping.
fn build_metadata_from_raw(meta: &RawMetadata, dims: &RawImage) -> Result<Metadata, DecodeError> {
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

/// RAW metadata via rawler (dimensions from a `dummy` decode; no pixel work).
/// Reads a sequential file prefix rather than mmap-faulting through the file —
/// see `source::with_ingest_source` for why that matters on slow disks.
fn read_metadata_raw(path: &Path) -> Result<Metadata, DecodeError> {
    crate::source::with_ingest_source(path, false, |src| {
        let decoder = rawler::get_decoder(src).map_err(rawler_err)?;
        let params = RawDecodeParams::default();

        let meta = decoder.raw_metadata(src, &params).map_err(rawler_err)?;
        // `dummy = true`: geometry only, no pixel decode (fast on an in-memory source).
        let dims = decoder.raw_image(src, &params, true).map_err(rawler_err)?;

        build_metadata_from_raw(&meta, &dims)
    })
    .map(|(meta, _probe)| meta)
}
