//! Read queries as free functions over a borrowed `&Connection`, so both the
//! writer (`Catalog`) and the read pool (`ReadPool`) share one implementation.

use crate::error::CatalogError;
use crate::model::{BackfillCandidate, DecodeStatus, ImageRecord};
use crate::thumbnail::Thumbnail;
use ferrolite_image::{Color, FileKind, Flag, Orientation, Rating, TagId};
use rusqlite::{Connection, OptionalExtension};
use std::path::PathBuf;

pub(crate) fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImageRecord> {
    let orientation_exif: Option<i64> = row.get(5)?;
    let status: i64 = row.get(8)?;
    let kind: i64 = row.get(9)?;
    let rating: i64 = row.get(10)?;
    let flag: i64 = row.get(11)?;
    let has_edits: i64 = row.get(12)?;
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
        has_edits: has_edits != 0,
    })
}

pub(crate) const IMAGE_COLS: &str = "id, folder_id, filename, width, height, orientation,
                          capture_time, iso, decode_status, kind, rating, flag, has_edits";

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

/// Batch-fetch collection ids for a slice of image ids. image_id -> collection_ids.
pub(crate) fn collections_for_images(
    conn: &Connection,
    image_ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<i64>>, CatalogError> {
    let mut map: std::collections::HashMap<i64, Vec<i64>> = std::collections::HashMap::new();
    if image_ids.is_empty() {
        return Ok(map);
    }
    let placeholders = vec!["?"; image_ids.len()].join(",");
    let sql = format!(
        "SELECT image_id, collection_id FROM collection_images WHERE image_id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(image_ids.iter()), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    for r in rows {
        let (img, coll) = r?;
        map.entry(img).or_default().push(coll);
    }
    Ok(map)
}

pub(crate) fn collection_image_counts(
    conn: &Connection,
) -> Result<std::collections::HashMap<i64, usize>, CatalogError> {
    let mut map: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    let mut stmt = conn
        .prepare("SELECT collection_id, COUNT(*) FROM collection_images GROUP BY collection_id")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, usize>(1)?))
    })?;
    for r in rows {
        let (coll_id, count) = r?;
        map.insert(coll_id, count);
    }
    Ok(map)
}

pub(crate) fn list_collections(
    conn: &Connection,
) -> Result<Vec<crate::model::CollectionRecord>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, color, sort_order, parent_id FROM collections ORDER BY sort_order, name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(crate::model::CollectionRecord {
            id: row.get(0)?,
            name: row.get(1)?,
            color: Color::from_packed(row.get::<_, i64>(2)? as u32),
            sort_order: row.get(3)?,
            parent_id: row.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn update_collection_parent(
    conn: &Connection,
    id: i64,
    parent_id: Option<i64>,
) -> Result<(), CatalogError> {
    conn.execute(
        "UPDATE collections SET parent_id = ?1 WHERE id = ?2",
        rusqlite::params![parent_id, id],
    )?;
    Ok(())
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

pub(crate) fn list_export_queue(conn: &Connection) -> Result<Vec<i64>, CatalogError> {
    let mut stmt = conn.prepare("SELECT image_id FROM export_queue ORDER BY position ASC")?;
    let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub(crate) fn images_by_ids(
    conn: &Connection,
    ids: &[i64],
) -> Result<Vec<ImageRecord>, CatalogError> {
    // Preserve the input order; skip ids that no longer exist.
    let mut out = Vec::with_capacity(ids.len());
    let mut stmt = conn.prepare(&format!("SELECT {IMAGE_COLS} FROM images WHERE id = ?1"))?;
    for &id in ids {
        let mut rows = stmt.query_map(rusqlite::params![id], row_to_record)?;
        if let Some(r) = rows.next() {
            out.push(r?);
        }
    }
    Ok(out)
}

/// Images whose `lens`, `aperture`, and `focal_length` are ALL still NULL —
/// the Task-14 background-backfill backlog: exactly the pre-v7-ingest set
/// (see `schema.rs`'s v7 migration note) plus any row a backfill pass hasn't
/// reached yet. A row that WAS attempted and found nothing is written back
/// with `lens = ''` (empty string, not NULL — see
/// `Catalog::apply_metadata_backfill_batch`), so it no longer has `lens IS
/// NULL` and drops out of this predicate on its own — it is never retried on
/// a later launch.
///
/// `after_id` + `ORDER BY images.id ASC` makes repeated calls within one
/// backfill job walk the backlog forward deterministically. This matters
/// because the write-back for a batch happens later, on the UI thread (see
/// `ferrolite-app`'s `meta_backfill` job) — without the id cursor, a second
/// call issued before that write lands would just re-fetch the same NULL
/// rows instead of making progress.
pub(crate) fn images_needing_metadata_backfill(
    conn: &Connection,
    after_id: i64,
    limit: i64,
) -> Result<Vec<BackfillCandidate>, CatalogError> {
    let mut stmt = conn.prepare(
        "SELECT images.id, folders.path, images.filename, images.kind
         FROM images JOIN folders ON folders.id = images.folder_id
         WHERE images.id > ?1
           AND images.lens IS NULL AND images.aperture IS NULL AND images.focal_length IS NULL
         ORDER BY images.id ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![after_id, limit], |row| {
        let folder_path: String = row.get(1)?;
        let filename: String = row.get(2)?;
        let kind: i64 = row.get(3)?;
        Ok(BackfillCandidate {
            id: row.get(0)?,
            path: PathBuf::from(folder_path).join(filename),
            kind: FileKind::from_i64(kind),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Count of rows still awaiting the Task-14 backfill (same predicate as
/// `images_needing_metadata_backfill`). Used for the one-shot startup gate:
/// the app only spawns the backfill job when this is `> 0`, so a fully
/// backfilled catalog pays zero extra job submissions on later launches.
pub(crate) fn metadata_backfill_pending_count(conn: &Connection) -> Result<i64, CatalogError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM images
         WHERE lens IS NULL AND aperture IS NULL AND focal_length IS NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(n)
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
