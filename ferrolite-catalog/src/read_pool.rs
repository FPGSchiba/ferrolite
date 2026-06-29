//! A small pool of read-only SQLite connections for UI queries. Under WAL these
//! never block the single writer. Source of truth is still the files on disk.

use crate::error::CatalogError;
use crate::model::ImageRecord;
use crate::thumbnail::Thumbnail;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct ReadPool {
    path: PathBuf,
    conns: Mutex<Vec<Connection>>,
}

fn open_read_only(path: &Path) -> Result<Connection, CatalogError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    Ok(conn)
}

impl ReadPool {
    /// Open `size` read-only connections to an existing catalog file. The writer
    /// (`Catalog::open`) must have created/migrated the file first.
    pub fn open(path: &Path, size: usize) -> Result<Self, CatalogError> {
        let mut conns = Vec::with_capacity(size.max(1));
        for _ in 0..size.max(1) {
            conns.push(open_read_only(path)?);
        }
        Ok(Self {
            path: path.to_path_buf(),
            conns: Mutex::new(conns),
        })
    }

    fn with_conn<R>(
        &self,
        f: impl FnOnce(&Connection) -> Result<R, CatalogError>,
    ) -> Result<R, CatalogError> {
        // Check out (or open a spare if the pool is momentarily drained).
        let conn = {
            let mut pool = self.conns.lock().expect("read pool mutex");
            pool.pop()
        };
        let conn = match conn {
            Some(c) => c,
            None => open_read_only(&self.path)?,
        };
        let result = f(&conn);
        self.conns.lock().expect("read pool mutex").push(conn);
        result
    }

    pub fn list_images(&self, folder_id: i64) -> Result<Vec<ImageRecord>, CatalogError> {
        self.with_conn(|c| crate::queries::list_images(c, folder_id))
    }
    pub fn list_images_recursive(&self, folder_id: i64) -> Result<Vec<ImageRecord>, CatalogError> {
        self.with_conn(|c| crate::queries::list_images_recursive(c, folder_id))
    }
    pub fn image_count(&self) -> Result<u64, CatalogError> {
        self.with_conn(crate::queries::image_count)
    }
    pub fn get_thumbnail(&self, image_id: i64) -> Result<Option<Thumbnail>, CatalogError> {
        self.with_conn(|c| crate::queries::get_thumbnail(c, image_id))
    }
    pub fn needs_reingest(
        &self,
        folder_id: i64,
        filename: &str,
        mtime: i64,
        size: i64,
    ) -> Result<bool, CatalogError> {
        self.with_conn(|c| crate::queries::needs_reingest(c, folder_id, filename, mtime, size))
    }
    pub fn list_folders(&self) -> Result<Vec<crate::FolderRecord>, CatalogError> {
        self.with_conn(crate::queries::list_folders)
    }
    pub fn folder_path(&self, folder_id: i64) -> Result<Option<String>, CatalogError> {
        self.with_conn(|c| crate::queries::folder_path(c, folder_id))
    }
}
