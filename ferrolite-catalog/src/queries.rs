//! Read queries as free functions over a borrowed `&Connection`, so both the
//! writer (`Catalog`) and the read pool (`ReadPool`) share one implementation.

use crate::error::CatalogError;
use crate::model::{DecodeStatus, ImageRecord};
use crate::thumbnail::Thumbnail;
use ferrolite_image::Orientation;
use rusqlite::{Connection, OptionalExtension};

pub(crate) fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImageRecord> {
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

const IMAGE_COLS: &str = "id, folder_id, filename, width, height, orientation,
                          capture_time, iso, decode_status";

pub(crate) fn list_images(
    conn: &Connection,
    folder_id: i64,
) -> Result<Vec<ImageRecord>, CatalogError> {
    let sql = format!("SELECT {IMAGE_COLS} FROM images WHERE folder_id = ?1 ORDER BY filename");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![folder_id], row_to_record)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn image_by_name(
    conn: &Connection,
    folder_id: i64,
    filename: &str,
) -> Result<Option<ImageRecord>, CatalogError> {
    let sql = format!("SELECT {IMAGE_COLS} FROM images WHERE folder_id = ?1 AND filename = ?2");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(rusqlite::params![folder_id, filename], row_to_record)?;
    Ok(match rows.next() {
        Some(r) => Some(r?),
        None => None,
    })
}

pub(crate) fn image_count(conn: &Connection) -> Result<u64, CatalogError> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM images", [], |row| row.get(0))?;
    Ok(n as u64)
}

pub(crate) fn needs_reingest(
    conn: &Connection,
    folder_id: i64,
    filename: &str,
    mtime: i64,
    size: i64,
) -> Result<bool, CatalogError> {
    let existing: Option<(i64, i64)> = conn
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

pub(crate) fn get_thumbnail(
    conn: &Connection,
    image_id: i64,
) -> Result<Option<Thumbnail>, CatalogError> {
    let mut stmt = conn.prepare("SELECT w, h, format, blob FROM thumbnails WHERE image_id = ?1")?;
    let mut rows = stmt.query_map(rusqlite::params![image_id], |row| {
        Ok(Thumbnail {
            width: row.get::<_, i64>(0)? as u32,
            height: row.get::<_, i64>(1)? as u32,
            format: row.get(2)?,
            bytes: row.get(3)?,
        })
    })?;
    Ok(match rows.next() {
        Some(t) => Some(t?),
        None => None,
    })
}

pub(crate) fn list_folders(conn: &Connection) -> Result<Vec<crate::FolderRecord>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path, COUNT(i.id)
         FROM folders f LEFT JOIN images i ON i.folder_id = f.id
         GROUP BY f.id, f.path ORDER BY f.path",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(crate::FolderRecord {
            id: row.get(0)?,
            path: row.get(1)?,
            image_count: row.get::<_, i64>(2)? as u64,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}
