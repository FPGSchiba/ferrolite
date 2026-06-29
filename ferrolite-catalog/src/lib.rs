//! SQLite digital-asset-management catalog: schema, ingest, thumbnails, queries.

mod catalog;
mod error;
mod ingest;
mod model;
mod queries;
mod read_pool;
mod scan;
mod schema;
mod thumbnail;

pub use catalog::Catalog;
pub use error::CatalogError;
pub use model::{DecodeStatus, ImageRecord, IngestSummary, NewImage};
pub use read_pool::ReadPool;
pub use scan::{is_raw, scan_raw_files, RawFile};
pub use schema::SCHEMA_VERSION;
pub use thumbnail::{generate_thumbnail, Thumbnail, ThumbnailStore, THUMB_MAX_EDGE, THUMB_QUALITY};

/// A folder with its image count (left-panel tree row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRecord {
    pub id: i64,
    pub path: String,
    pub image_count: u64,
}
