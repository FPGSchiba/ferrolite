//! ferrolite-vt — source-agnostic sparse virtual texture. Engine-transferable.

mod histogram;
mod lod;
mod page_table;
mod pipelines;
mod pool;
mod present;
mod producer;
mod residency;
mod source;
mod transform;
mod view;

pub use histogram::{bin_index, HistogramPipeline, HIST_BINS, HIST_CHANNELS, HIST_LEN};
pub use lod::lod_levels;
pub use page_table::{FeedbackBuffer, LevelLayout, PageTable};
pub use pipelines::{DisplayPipelines, DisplayVariant, LUT_SIZE};
pub use pool::{SlotAllocator, TilePool, NOT_RESIDENT};
pub use present::PresentBuffers;
pub use producer::TileProducer;
pub use residency::{needed_tiles, needed_tiles_prefetched, ResidencySet, VersionedResidency};
pub use source::{PyramidTileSource, TileSource};
pub use transform::ViewTransform;
pub use view::{live_virtual_textures, VirtualTexture};
