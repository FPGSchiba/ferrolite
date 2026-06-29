//! ferrolite-vt — source-agnostic sparse virtual texture. Engine-transferable.

mod residency;
mod source;
mod transform;

pub use residency::{needed_tiles, ResidencySet};
pub use source::{PyramidTileSource, TileSource};
pub use transform::ViewTransform;
