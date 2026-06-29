//! Core pixel/orientation vocabulary shared across ferrolite crates.

mod file_kind;
mod orientation;
mod pixel;

pub use file_kind::FileKind;
pub use orientation::Orientation;
pub use pixel::{ImageBuffer, ImageBufferError, PixelFormat};
