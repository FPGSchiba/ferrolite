//! SQLite digital-asset-management catalog: schema, ingest, thumbnails, queries.

mod catalog;
mod error;
mod model;
mod schema;
mod thumbnail;

pub use catalog::Catalog;
pub use error::CatalogError;
pub use model::{DecodeStatus, ImageRecord, IngestSummary, NewImage};
pub use schema::SCHEMA_VERSION;
pub use thumbnail::{generate_thumbnail, Thumbnail, ThumbnailStore, THUMB_MAX_EDGE, THUMB_QUALITY};
