use crate::error::CatalogError;
use crate::schema;
use rusqlite::Connection;
use std::path::Path;

/// SQLite-backed catalog. The catalog is a *cache*: source of truth is the files
/// on disk. A corrupt/missing DB is always rebuildable by re-ingesting.
pub struct Catalog {
    conn: Connection,
}

impl Catalog {
    pub fn open(path: &Path) -> Result<Self, CatalogError> {
        let conn = Connection::open(path)?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, CatalogError> {
        let conn = Connection::open_in_memory()?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn schema_version(&self) -> Result<i64, CatalogError> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    #[allow(dead_code)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}
