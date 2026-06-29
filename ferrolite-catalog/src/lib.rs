//! SQLite digital-asset-management catalog: schema, ingest, thumbnails, queries.

mod catalog;
mod error;
mod model;
mod schema;

pub use catalog::Catalog;
pub use error::CatalogError;
pub use model::{DecodeStatus, ImageRecord, IngestSummary, NewImage};
pub use schema::SCHEMA_VERSION;
