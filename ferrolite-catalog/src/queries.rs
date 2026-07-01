//! Read queries as free functions over a borrowed `&Connection`, so both the
//! writer (`Catalog`) and the read pool (`ReadPool`) share one implementation.

use crate::error::CatalogError;
use crate::model::{DecodeStatus, ImageRecord};
use crate::thumbnail::Thumbnail;
use ferrolite_image::{Color, FileKind, Flag, Orientation, Rating, TagId};
use rusqlite::{Connection, OptionalExtension};

pub(crate) fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImageRecord> {
    let orientation_exif: Option<i64> = row.get(5)?;
    let status: i64 = row.get(8)?;
    let kind: i64 = row.get(9)?;
    let rating: i64 = row.get(10)?;
    let flag: i64 = row.get(11)?;
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
        kind: FileKind::from_i64(kind),
        rating: Rating::from_i64(rating),
        flag: Flag::from_i64(flag),
    })
}

pub(crate) const IMAGE_COLS: &str = "id, folder_id, filename, width, height, orientation,
                          capture_time, iso, decode_status, kind, rating, flag";

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

pub(crate) fn folder_path(
    conn: &Connection,
    folder_id: i64,
) -> Result<Option<String>, CatalogError> {
    let p = conn
        .query_row(
            "SELECT path FROM folders WHERE id = ?1",
            rusqlite::params![folder_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(p)
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
    let existing: Option<(i64, i64, i64)> = conn
        .query_row(
            "SELECT mtime, size, decode_status FROM images \
             WHERE folder_id = ?1 AND filename = ?2",
            rusqlite::params![folder_id, filename],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(match existing {
        // Reingest when the file changed OR the row is still a stat-only
        // placeholder from the instant index pass (metadata not yet read).
        Some((m, s, status)) => m != mtime || s != size || status == DecodeStatus::Pending.as_i64(),
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

pub(crate) fn list_images_recursive(
    conn: &Connection,
    folder_id: i64,
) -> Result<Vec<ImageRecord>, CatalogError> {
    let sql = format!(
        "WITH RECURSIVE subtree(id) AS (
             SELECT id FROM folders WHERE id = ?1
             UNION ALL
             SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id
         )
         SELECT {IMAGE_COLS} FROM images
         WHERE folder_id IN (SELECT id FROM subtree)
         ORDER BY filename"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![folder_id], row_to_record)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn list_tags(conn: &Connection) -> Result<Vec<crate::model::TagRecord>, CatalogError> {
    let mut stmt = conn.prepare("SELECT id, name, color FROM tags ORDER BY name")?;
    let rows = stmt.query_map([], |row| {
        Ok(crate::model::TagRecord {
            id: TagId(row.get(0)?),
            name: row.get(1)?,
            color: Color::from_packed(row.get::<_, i64>(2)? as u32),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn tags_for_images(
    conn: &Connection,
    image_ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<TagId>>, CatalogError> {
    let mut map: std::collections::HashMap<i64, Vec<TagId>> = std::collections::HashMap::new();
    if image_ids.is_empty() {
        return Ok(map);
    }
    let placeholders = vec!["?"; image_ids.len()].join(",");
    let sql = format!(
        "SELECT image_id, tag_id FROM image_tags WHERE image_id IN ({placeholders}) ORDER BY tag_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(image_ids.iter()), |row| {
        Ok((row.get::<_, i64>(0)?, TagId(row.get::<_, i64>(1)?)))
    })?;
    for r in rows {
        let (img, tag) = r?;
        map.entry(img).or_default().push(tag);
    }
    Ok(map)
}

pub(crate) fn list_collections(
    conn: &Connection,
) -> Result<Vec<crate::model::CollectionRecord>, CatalogError> {
    let mut stmt = conn
        .prepare("SELECT id, name, color, sort_order FROM collections ORDER BY sort_order, name")?;
    let rows = stmt.query_map([], |row| {
        Ok(crate::model::CollectionRecord {
            id: row.get(0)?,
            name: row.get(1)?,
            color: Color::from_packed(row.get::<_, i64>(2)? as u32),
            sort_order: row.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn list_folders(conn: &Connection) -> Result<Vec<crate::FolderRecord>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path, f.parent_id, COUNT(i.id)
         FROM folders f LEFT JOIN images i ON i.folder_id = f.id
         GROUP BY f.id, f.path, f.parent_id ORDER BY f.path",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(crate::FolderRecord {
            id: row.get(0)?,
            path: row.get(1)?,
            parent_id: row.get(2)?,
            image_count: row.get::<_, i64>(3)? as u64,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn distinct_cameras(conn: &Connection) -> Result<Vec<String>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT camera_model FROM images WHERE camera_model IS NOT NULL ORDER BY camera_model",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn iso_bounds(conn: &Connection) -> Result<Option<(u32, u32)>, CatalogError> {
    let row: (Option<i64>, Option<i64>) = conn.query_row(
        "SELECT MIN(iso), MAX(iso) FROM images WHERE iso IS NOT NULL",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(match row {
        (Some(lo), Some(hi)) => Some((lo as u32, hi as u32)),
        _ => None,
    })
}

pub(crate) fn date_bounds(conn: &Connection) -> Result<Option<(String, String)>, CatalogError> {
    let row: (Option<String>, Option<String>) = conn.query_row(
        "SELECT MIN(capture_time), MAX(capture_time) FROM images WHERE capture_time IS NOT NULL",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(match row {
        (Some(lo), Some(hi)) => Some((lo, hi)),
        _ => None,
    })
}
