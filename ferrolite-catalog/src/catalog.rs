use crate::error::CatalogError;
use crate::model::{DecodeStatus, ImageRecord, NewImage};
use crate::schema;
use rusqlite::Connection;
use std::collections::HashSet;
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
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, CatalogError> {
        let conn = Connection::open_in_memory()?;
        schema::migrate(&conn)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
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

    /// Insert a folder by path with an optional parent, or return the existing
    /// id. A non-null `parent_id` overwrites; a `None` keeps any existing parent
    /// (so re-opening a subfolder as a root does not orphan its wired parent).
    pub fn upsert_folder(&self, path: &Path, parent_id: Option<i64>) -> Result<i64, CatalogError> {
        let p = path.to_string_lossy();
        self.conn().execute(
            "INSERT INTO folders (path, parent_id) VALUES (?1, ?2)
             ON CONFLICT(path) DO UPDATE SET parent_id = COALESCE(?2, parent_id)",
            rusqlite::params![p, parent_id],
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
                width, height, orientation, capture_time, iso, decode_status, kind,
                rating, added_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
             ON CONFLICT(folder_id, filename) DO UPDATE SET
                mtime=?3, size=?4, camera_make=?5, camera_model=?6, width=?7,
                height=?8, orientation=?9, capture_time=?10, iso=?11,
                decode_status=?12, kind=?13, rating=?14",
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
                img.kind.as_i64(),
                img.rating.as_i64(),
                img.added_at,
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

    pub fn list_images_recursive(&self, folder_id: i64) -> Result<Vec<ImageRecord>, CatalogError> {
        crate::queries::list_images_recursive(self.conn(), folder_id)
    }

    /// Prune a folder subtree to mirror disk after a full rescan: delete images
    /// (and their thumbnails) whose id is not in `kept_image_ids`, and folders
    /// (vanished subdirectories) whose id is not in `kept_folder_ids`. The root
    /// is expected to be in `kept_folder_ids` and is never pruned. Cache only —
    /// never touches files on disk. Runs in one transaction.
    pub fn prune_subtree(
        &self,
        root_folder_id: i64,
        kept_folder_ids: &HashSet<i64>,
        kept_image_ids: &HashSet<i64>,
    ) -> Result<(), CatalogError> {
        const SUBTREE_CTE: &str = "WITH RECURSIVE subtree(id) AS (
                 SELECT id FROM folders WHERE id = ?1
                 UNION ALL
                 SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id
             )";
        let tx = self.conn().unchecked_transaction()?;

        let subtree_image_ids: Vec<i64> = {
            let sql = format!(
                "{SUBTREE_CTE} SELECT id FROM images WHERE folder_id IN (SELECT id FROM subtree)"
            );
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params![root_folder_id], |r| r.get::<_, i64>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for img in subtree_image_ids {
            if !kept_image_ids.contains(&img) {
                tx.execute(
                    "DELETE FROM thumbnails WHERE image_id = ?1",
                    rusqlite::params![img],
                )?;
                tx.execute("DELETE FROM images WHERE id = ?1", rusqlite::params![img])?;
            }
        }

        let subtree_folder_ids: Vec<i64> = {
            let sql = format!("{SUBTREE_CTE} SELECT id FROM subtree");
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params![root_folder_id], |r| r.get::<_, i64>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for fid in subtree_folder_ids {
            if !kept_folder_ids.contains(&fid) {
                tx.execute("DELETE FROM folders WHERE id = ?1", rusqlite::params![fid])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Delete a folder subtree from the catalog (thumbnails → images → folders),
    /// in one transaction. Cache only — never touches files on disk.
    pub fn remove_folder(&self, folder_id: i64) -> Result<(), CatalogError> {
        let tx = self.conn().unchecked_transaction()?;
        tx.execute(
            "DELETE FROM thumbnails WHERE image_id IN (
                 WITH RECURSIVE subtree(id) AS (
                     SELECT id FROM folders WHERE id = ?1
                     UNION ALL
                     SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id
                 )
                 SELECT id FROM images WHERE folder_id IN (SELECT id FROM subtree))",
            rusqlite::params![folder_id],
        )?;
        tx.execute(
            "DELETE FROM images WHERE folder_id IN (
                 WITH RECURSIVE subtree(id) AS (
                     SELECT id FROM folders WHERE id = ?1
                     UNION ALL
                     SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id
                 )
                 SELECT id FROM subtree)",
            rusqlite::params![folder_id],
        )?;
        tx.execute(
            "DELETE FROM folders WHERE id IN (
                 WITH RECURSIVE subtree(id) AS (
                     SELECT id FROM folders WHERE id = ?1
                     UNION ALL
                     SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id
                 )
                 SELECT id FROM subtree)",
            rusqlite::params![folder_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn folder_path(&self, folder_id: i64) -> Result<Option<String>, CatalogError> {
        crate::queries::folder_path(self.conn(), folder_id)
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

#[cfg(test)]
mod rating_tests {
    use super::*;
    use crate::model::NewImage;
    use ferrolite_image::{FileKind, Rating};

    #[test]
    fn upsert_persists_rating_and_added_at_then_refreshes_rating_only() {
        let cat = Catalog::open_in_memory().unwrap();
        let f = cat.upsert_folder(std::path::Path::new("/p"), None).unwrap();
        let mut img = NewImage::failed(f, "a.nef".into(), 1, 1, FileKind::Raw, 1000);
        img.rating = Rating::new(3);
        let id = cat.upsert_image(&img).unwrap();

        let rec = cat.list_images(f).unwrap().into_iter().next().unwrap();
        assert_eq!(rec.rating, Rating::new(3));
        assert_eq!(rec.flag, ferrolite_image::Flag::None);

        // Re-upsert with a new rating + later added_at: rating updates, added_at is preserved.
        let mut img2 = NewImage::failed(f, "a.nef".into(), 2, 2, FileKind::Raw, 9999);
        img2.rating = Rating::new(5);
        cat.upsert_image(&img2).unwrap();
        let added: Option<i64> = cat
            .conn()
            .query_row("SELECT added_at FROM images WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(added, Some(1000), "added_at preserved on conflict");
        let rec = cat.list_images(f).unwrap().into_iter().next().unwrap();
        assert_eq!(rec.rating, Rating::new(5), "rating refreshed on conflict");
    }
}
