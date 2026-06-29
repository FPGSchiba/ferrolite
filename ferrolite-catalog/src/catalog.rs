use crate::error::CatalogError;
use crate::model::{DecodeStatus, ImageRecord, NewImage};
use crate::schema;
use ferrolite_image::Orientation;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
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
        let mut stmt = self.conn().prepare(
            "SELECT id, folder_id, filename, width, height, orientation,
                    capture_time, iso, decode_status
             FROM images WHERE folder_id = ?1 AND filename = ?2",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![folder_id, filename], row_to_record)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn list_images(&self, folder_id: i64) -> Result<Vec<ImageRecord>, CatalogError> {
        let mut stmt = self.conn().prepare(
            "SELECT id, folder_id, filename, width, height, orientation,
                    capture_time, iso, decode_status
             FROM images WHERE folder_id = ?1 ORDER BY filename",
        )?;
        let rows = stmt.query_map(rusqlite::params![folder_id], row_to_record)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn image_count(&self) -> Result<u64, CatalogError> {
        let n: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM images", [], |row| row.get(0))?;
        Ok(n as u64)
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
        let existing: Option<(i64, i64)> = self
            .conn()
            .query_row(
                "SELECT mtime, size FROM images WHERE folder_id = ?1 AND filename = ?2",
                rusqlite::params![folder_id, filename],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(match existing {
            Some((m, s)) => m != mtime || s != size,
            None => true,
        })
    }
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImageRecord> {
    let orientation_exif: Option<i64> = row.get(5)?;
    let status: i64 = row.get(8)?;
    Ok(ImageRecord {
        id: row.get(0)?,
        folder_id: row.get(1)?,
        filename: row.get(2)?,
        width: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
        height: row.get::<_, Option<i64>>(4)?.map(|v| v as u32),
        orientation: Orientation::from_exif(orientation_exif.unwrap_or(1) as u16),
        capture_time: row.get(6)?,
        iso: row.get::<_, Option<i64>>(7)?.map(|v| v as u32),
        decode_status: DecodeStatus::from_i64(status),
    })
}
