//! ferrolite-export — the photo-tier encode core. Renders the full-res edited
//! image TILED via the Spec 2 GPU tile producer (no whole-image RGBA16F),
//! converts working→output via ferrolite-color, optionally resizes, and encodes
//! JPEG/PNG/TIFF/WebP with EXIF copy + embedded ICC. Runs on ferrolite-jobs at
//! Background priority (spec §8).

mod convert;
mod encode;
mod error;
mod job;
mod metadata;
mod options;
mod render;
mod resize;

pub use error::ExportError;
pub use options::{BitDepth, ExportFormat, ExportOptions, ResizeSpec};
pub use render::{PixelData, RenderedImage};

// (job module wiring — render_tiled/run_export/ExportRequest/ExportOutcome —
// added in Tasks 6/9)
