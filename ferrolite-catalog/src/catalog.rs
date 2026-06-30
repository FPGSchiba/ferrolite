use crate::error::CatalogError;
use crate::model::{DecodeStatus, ImageRecord, NewImage};
use crate::schema;
use ferrolite_image::{Color, TagId};
use rusqlite::Connection;
use std::collections::HashMap;
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

    /// Update the rating of an image row.
    pub fn set_rating(
        &self,
        image_id: i64,
        rating: ferrolite_image::Rating,
    ) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE images SET rating=?1 WHERE id=?2",
            rusqlite::params![rating.as_i64(), image_id],
        )?;
        Ok(())
    }

    /// Update the flag of an image row.
    pub fn set_flag(&self, image_id: i64, flag: ferrolite_image::Flag) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE images SET flag=?1 WHERE id=?2",
            rusqlite::params![flag.as_i64(), image_id],
        )?;
        Ok(())
    }

    /// Add the tag if absent, else remove it (mirrors the UI toggle).
    pub fn toggle_tag(
        &self,
        image_id: i64,
        tag: ferrolite_image::TagId,
    ) -> Result<(), CatalogError> {
        let present: bool = self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM image_tags WHERE image_id=?1 AND tag_id=?2)",
            rusqlite::params![image_id, tag.0],
            |r| r.get(0),
        )?;
        if present {
            self.remove_tag_from_image(image_id, tag)
        } else {
            self.add_tag_to_image(image_id, tag)
        }
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

    /// Create a new tag. Returns `CatalogError::Conflict` if a tag with that name
    /// already exists (enforced by the UNIQUE constraint on `tags.name`).
    pub fn create_tag(&self, name: &str, color: Color) -> Result<TagId, CatalogError> {
        let res = self.conn().execute(
            "INSERT INTO tags (name, color) VALUES (?1, ?2)",
            rusqlite::params![name, color.to_packed() as i64],
        );
        match res {
            Ok(_) => Ok(TagId(self.conn().last_insert_rowid())),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(CatalogError::Conflict(format!(
                    "tag '{name}' already exists"
                )))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Rename an existing tag.
    pub fn rename_tag(&self, id: TagId, name: &str) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE tags SET name=?1 WHERE id=?2",
            rusqlite::params![name, id.0],
        )?;
        Ok(())
    }

    /// Update the colour of an existing tag.
    pub fn set_tag_color(&self, id: TagId, color: Color) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE tags SET color=?1 WHERE id=?2",
            rusqlite::params![color.to_packed() as i64, id.0],
        )?;
        Ok(())
    }

    /// Delete a tag and cascade-remove all its image associations.
    pub fn delete_tag(&self, id: TagId) -> Result<(), CatalogError> {
        self.conn()
            .execute("DELETE FROM tags WHERE id=?1", rusqlite::params![id.0])?;
        Ok(())
    }

    /// Return all tags ordered by name.
    pub fn list_tags(&self) -> Result<Vec<crate::model::TagRecord>, CatalogError> {
        crate::queries::list_tags(self.conn())
    }

    /// Associate a tag with an image. Idempotent (INSERT OR IGNORE).
    pub fn add_tag_to_image(&self, image_id: i64, tag: TagId) -> Result<(), CatalogError> {
        self.conn().execute(
            "INSERT OR IGNORE INTO image_tags (image_id, tag_id) VALUES (?1, ?2)",
            rusqlite::params![image_id, tag.0],
        )?;
        Ok(())
    }

    /// Remove a tag association from an image.
    pub fn remove_tag_from_image(&self, image_id: i64, tag: TagId) -> Result<(), CatalogError> {
        self.conn().execute(
            "DELETE FROM image_tags WHERE image_id=?1 AND tag_id=?2",
            rusqlite::params![image_id, tag.0],
        )?;
        Ok(())
    }

    /// Batch-fetch tag ids for a slice of image ids. Used by the virtualized grid
    /// to load tag associations for the currently-visible rows in one query.
    pub fn tags_for_images(
        &self,
        image_ids: &[i64],
    ) -> Result<HashMap<i64, Vec<TagId>>, CatalogError> {
        crate::queries::tags_for_images(self.conn(), image_ids)
    }

    /// Create a new collection. Returns `CatalogError::Conflict` if a collection
    /// with that name already exists (enforced by the UNIQUE constraint on `collections.name`).
    pub fn create_collection(&self, name: &str, color: Color) -> Result<i64, CatalogError> {
        let res = self.conn().execute(
            "INSERT INTO collections (name, color) VALUES (?1, ?2)",
            rusqlite::params![name, color.to_packed() as i64],
        );
        match res {
            Ok(_) => Ok(self.conn().last_insert_rowid()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(CatalogError::Conflict(format!(
                    "collection '{name}' already exists"
                )))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Rename an existing collection.
    pub fn rename_collection(&self, id: i64, name: &str) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE collections SET name=?1 WHERE id=?2",
            rusqlite::params![name, id],
        )?;
        Ok(())
    }

    /// Update the colour of an existing collection.
    pub fn set_collection_color(&self, id: i64, color: Color) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE collections SET color=?1 WHERE id=?2",
            rusqlite::params![color.to_packed() as i64, id],
        )?;
        Ok(())
    }

    /// Delete a collection and cascade-remove all its image memberships.
    pub fn delete_collection(&self, id: i64) -> Result<(), CatalogError> {
        self.conn()
            .execute("DELETE FROM collections WHERE id=?1", rusqlite::params![id])?;
        Ok(())
    }

    /// Add an image to a collection. Idempotent (INSERT OR IGNORE).
    pub fn add_image_to_collection(&self, coll_id: i64, image_id: i64) -> Result<(), CatalogError> {
        self.conn().execute(
            "INSERT OR IGNORE INTO collection_images (collection_id, image_id) VALUES (?1, ?2)",
            rusqlite::params![coll_id, image_id],
        )?;
        Ok(())
    }

    /// Remove an image from a collection.
    pub fn remove_image_from_collection(
        &self,
        coll_id: i64,
        image_id: i64,
    ) -> Result<(), CatalogError> {
        self.conn().execute(
            "DELETE FROM collection_images WHERE collection_id=?1 AND image_id=?2",
            rusqlite::params![coll_id, image_id],
        )?;
        Ok(())
    }

    /// Return all collections ordered by sort_order then name.
    pub fn list_collections(&self) -> Result<Vec<crate::model::CollectionRecord>, CatalogError> {
        crate::queries::list_collections(self.conn())
    }

    /// Execute a `LibraryQuery` and return matching image records.
    pub fn query_images(&self, q: &crate::LibraryQuery) -> Result<Vec<ImageRecord>, CatalogError> {
        crate::query::run(self.conn(), q)
    }
}

#[cfg(test)]
mod collection_tests {
    use super::*;
    use crate::model::NewImage;
    use ferrolite_image::{Color, FileKind};

    #[test]
    fn create_and_populate_collection() {
        let cat = Catalog::open_in_memory().unwrap();
        let c = cat
            .create_collection("Best of 2026", Color::from_packed(0x30A46C))
            .unwrap();
        assert!(cat
            .create_collection("Best of 2026", Color::default())
            .is_err());
        let f = cat.upsert_folder(std::path::Path::new("/p"), None).unwrap();
        let a = cat
            .upsert_image(&NewImage::failed(f, "a.nef".into(), 1, 1, FileKind::Raw, 0))
            .unwrap();
        cat.add_image_to_collection(c, a).unwrap();
        cat.add_image_to_collection(c, a).unwrap(); // idempotent
        let n: i64 = cat
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM collection_images WHERE collection_id=?1",
                [c],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(cat.list_collections().unwrap().len(), 1);
        cat.delete_collection(c).unwrap();
        assert!(cat.list_collections().unwrap().is_empty());
    }
}

#[cfg(test)]
mod tag_tests {
    use super::*;
    use crate::model::NewImage;
    use ferrolite_image::{Color, FileKind};

    #[test]
    fn create_list_and_associate_tags() {
        let cat = Catalog::open_in_memory().unwrap();
        let red = cat
            .create_tag("portrait", Color::from_packed(0xE5484D))
            .unwrap();
        let green = cat
            .create_tag("keeper", Color::from_packed(0x30A46C))
            .unwrap();
        assert!(
            cat.create_tag("portrait", Color::default()).is_err(),
            "dup name errors"
        );

        let tags = cat.list_tags().unwrap();
        assert_eq!(tags.len(), 2);

        let f = cat.upsert_folder(std::path::Path::new("/p"), None).unwrap();
        let a = cat
            .upsert_image(&NewImage::failed(f, "a.nef".into(), 1, 1, FileKind::Raw, 0))
            .unwrap();
        let b = cat
            .upsert_image(&NewImage::failed(f, "b.nef".into(), 1, 1, FileKind::Raw, 0))
            .unwrap();
        cat.add_tag_to_image(a, red).unwrap();
        cat.add_tag_to_image(a, green).unwrap();
        cat.add_tag_to_image(b, red).unwrap();
        cat.add_tag_to_image(a, red).unwrap(); // idempotent

        let map = cat.tags_for_images(&[a, b]).unwrap();
        assert_eq!(map.get(&a).unwrap().len(), 2);
        assert_eq!(map.get(&b).unwrap(), &vec![red]);

        cat.remove_tag_from_image(a, green).unwrap();
        assert_eq!(cat.tags_for_images(&[a]).unwrap().get(&a).unwrap().len(), 1);

        cat.delete_tag(red).unwrap();
        assert_eq!(cat.list_tags().unwrap().len(), 1);
        // cascade removed associations to `red`
        assert!(!cat.tags_for_images(&[b]).unwrap().contains_key(&b));
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
