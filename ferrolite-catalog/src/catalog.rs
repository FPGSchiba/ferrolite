use crate::error::CatalogError;
use crate::model::{DecodeStatus, ImageRecord, NewImage};
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
        // WAL lets the read pool query concurrently with the single writer.
        // (In-memory DBs ignore journal_mode; harmless there.)
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, CatalogError> {
        let conn = Connection::open_in_memory()?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn schema_version(&self) -> Result<i64, CatalogError> {
        Ok(self
            .conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Insert a folder by path, or return the existing id. Idempotent.
    pub fn upsert_folder(&self, path: &Path) -> Result<i64, CatalogError> {
        let p = path.to_string_lossy();
        self.conn().execute(
            "INSERT INTO folders (path) VALUES (?1) ON CONFLICT(path) DO NOTHING",
            rusqlite::params![p],
        )?;
        let id = self.conn().query_row(
            "SELECT id FROM folders WHERE path = ?1",
            rusqlite::params![p],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// Insert or update an image keyed by (folder_id, filename). Returns its id.
    pub fn upsert_image(&self, img: &NewImage) -> Result<i64, CatalogError> {
        self.conn().execute(
            "INSERT INTO images
               (folder_id, filename, mtime, size, camera_make, camera_model,
                width, height, orientation, capture_time, iso, decode_status)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(folder_id, filename) DO UPDATE SET
                mtime=?3, size=?4, camera_make=?5, camera_model=?6, width=?7,
                height=?8, orientation=?9, capture_time=?10, iso=?11, decode_status=?12",
            rusqlite::params![
                img.folder_id,
                img.filename,
                img.mtime,
                img.size,
                img.make,
                img.model,
                img.width,
                img.height,
                img.orientation.to_exif(),
                img.capture_time,
                img.iso,
                img.decode_status.as_i64(),
            ],
        )?;
        let id = self.conn().query_row(
            "SELECT id FROM images WHERE folder_id = ?1 AND filename = ?2",
            rusqlite::params![img.folder_id, img.filename],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    pub fn image_by_name(
        &self,
        folder_id: i64,
        filename: &str,
    ) -> Result<Option<ImageRecord>, CatalogError> {
        crate::queries::image_by_name(self.conn(), folder_id, filename)
    }

    pub fn list_images(&self, folder_id: i64) -> Result<Vec<ImageRecord>, CatalogError> {
        crate::queries::list_images(self.conn(), folder_id)
    }

    pub fn image_count(&self) -> Result<u64, CatalogError> {
        crate::queries::image_count(self.conn())
    }

    /// True when the file is new or its (mtime, size) differ from the catalog —
    /// the incremental-rescan skip check.
    pub fn needs_reingest(
        &self,
        folder_id: i64,
        filename: &str,
        mtime: i64,
        size: i64,
    ) -> Result<bool, CatalogError> {
        crate::queries::needs_reingest(self.conn(), folder_id, filename, mtime, size)
    }

    /// Set a row's decode status (used to mark a file `Failed` from a thumbnail job).
    pub fn set_decode_status(
        &self,
        image_id: i64,
        status: DecodeStatus,
    ) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE images SET decode_status = ?1 WHERE id = ?2",
            rusqlite::params![status.as_i64(), image_id],
        )?;
        Ok(())
    }
}
