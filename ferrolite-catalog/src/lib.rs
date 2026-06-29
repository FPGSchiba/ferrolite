//! SQLite digital-asset-management catalog: schema, ingest, thumbnails, queries.

mod catalog;
mod error;
mod schema;

pub use catalog::Catalog;
pub use error::CatalogError;
pub use schema::SCHEMA_VERSION;
