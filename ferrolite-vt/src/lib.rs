//! ferrolite-vt — source-agnostic sparse virtual texture. Engine-transferable.

mod page_table;
mod pool;
mod residency;
mod source;
mod transform;
mod view;

pub use page_table::{FeedbackBuffer, LevelLayout, PageTable};
pub use pool::{SlotAllocator, TilePool, NOT_RESIDENT};
pub use residency::{needed_tiles, ResidencySet};
pub use source::{PyramidTileSource, TileSource};
pub use transform::ViewTransform;
pub use view::VirtualTexture;
