//! ferrolite-export — the photo-tier encode core. Renders the full-res edited
//! image TILED via the Spec 2 GPU tile producer (no whole-image RGBA16F),
//! converts working→output via ferrolite-color, optionally resizes, and encodes
//! JPEG/PNG/TIFF/WebP with EXIF copy + embedded ICC. Runs on ferrolite-jobs at
//! Background priority (spec §8).

mod convert;
mod encode;
mod error;
pub mod filename;
pub mod job;
mod metadata;
mod options;
mod render;
mod resize;

pub use error::ExportError;
pub use filename::{
    expand as expand_filename, format_capture_date, resolve_collision, FilenameCtx,
};
pub use job::{run_export, ExportOutcome, ExportRequest};
pub use options::{BitDepth, ExportFormat, ExportOptions, ResizeSpec};
pub use render::{render_tiled, PixelData, RenderedImage};

/// Test-only re-export of the internal encoder so integration tests can encode a
/// `RenderedImage` without going through the GPU render path.
#[doc(hidden)]
pub fn encode_for_test(
    img: &RenderedImage,
    opts: &ExportOptions,
    dest: &std::path::Path,
) -> Result<Vec<String>, ExportError> {
    crate::encode::encode_to_file(img, opts, dest)
}
