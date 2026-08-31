use rusqlite::Connection;

/// Bump this and add a `if version < N { ... }` block when the schema changes.
pub const SCHEMA_VERSION: i64 = 8;

/// Apply migrations using the SQLite `user_version` pragma. Idempotent.
pub(crate) fn migrate(conn: &Connection) -> Result<(), rusqlite::Error> {
    let mut version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE folders (
                 id           INTEGER PRIMARY KEY,
                 path         TEXT NOT NULL UNIQUE,
                 parent_id    INTEGER,
                 last_scanned INTEGER
             );
             CREATE TABLE images (
                 id            INTEGER PRIMARY KEY,
                 folder_id     INTEGER NOT NULL REFERENCES folders(id),
                 filename      TEXT NOT NULL,
                 mtime         INTEGER NOT NULL,
                 size          INTEGER NOT NULL,
                 camera_make   TEXT,
                 camera_model  TEXT,
                 width         INTEGER,
                 height        INTEGER,
                 orientation   INTEGER,
                 capture_time  TEXT,
                 iso           INTEGER,
                 rating        INTEGER NOT NULL DEFAULT 0,
                 label         TEXT,
                 decode_status INTEGER NOT NULL DEFAULT 0,
                 UNIQUE(folder_id, filename)
             );
             CREATE INDEX idx_images_folder ON images(folder_id);
             CREATE INDEX idx_images_capture ON images(capture_time);
             CREATE TABLE thumbnails (
                 image_id INTEGER PRIMARY KEY REFERENCES images(id),
                 level    INTEGER NOT NULL,
                 w        INTEGER NOT NULL,
                 h        INTEGER NOT NULL,
                 format   TEXT NOT NULL,
                 blob     BLOB NOT NULL
             );",
        )?;
        version = 1;
    }

    if version < 2 {
        conn.execute_batch("ALTER TABLE images ADD COLUMN kind INTEGER NOT NULL DEFAULT 0;")?;
        version = 2;
    }

    if version < 3 {
        conn.execute_batch(
            // `flag`: 0 none, 1 pick, 2 reject. `added_at`: ingest epoch seconds.
            // `label` (from v1) is abandoned in place — no longer read or written.
            "ALTER TABLE images ADD COLUMN flag     INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE images ADD COLUMN added_at INTEGER;

             CREATE TABLE tags (
                 id    INTEGER PRIMARY KEY,
                 name  TEXT NOT NULL UNIQUE,
                 color INTEGER NOT NULL
             );
             CREATE TABLE image_tags (
                 image_id INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
                 tag_id   INTEGER NOT NULL REFERENCES tags(id)   ON DELETE CASCADE,
                 PRIMARY KEY (image_id, tag_id)
             );
             CREATE INDEX idx_image_tags_tag ON image_tags(tag_id);

             CREATE TABLE collections (
                 id         INTEGER PRIMARY KEY,
                 name       TEXT NOT NULL UNIQUE,
                 color      INTEGER NOT NULL,
                 sort_order INTEGER NOT NULL DEFAULT 0,
                 parent_id  INTEGER REFERENCES collections(id) ON DELETE CASCADE
             );
             CREATE TABLE collection_images (
                 collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
                 image_id      INTEGER NOT NULL REFERENCES images(id)      ON DELETE CASCADE,
                 position      INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (collection_id, image_id)
             );",
        )?;
        version = 3;
    }

    if version < 4 {
        conn.execute_batch("ALTER TABLE images ADD COLUMN has_edits INTEGER NOT NULL DEFAULT 0;")?;
        version = 4;
    }

    if version < 5 {
        // Persisted export queue (spec §8.4). This is UI-state CACHE, not
        // source-of-truth: ON DELETE CASCADE means a deleted image drops out of
        // the queue automatically, and losing the table never loses photos/edits.
        conn.execute_batch(
            "CREATE TABLE export_queue (
                 image_id  INTEGER NOT NULL PRIMARY KEY
                            REFERENCES images(id) ON DELETE CASCADE,
                 position  INTEGER NOT NULL,
                 added_at  INTEGER NOT NULL
             );
             CREATE INDEX idx_export_queue_position ON export_queue(position);",
        )?;
        version = 5;
    }

    if version < 6 {
        if !has_column(conn, "collections", "parent_id")? {
            conn.execute_batch(
                "ALTER TABLE collections ADD COLUMN parent_id INTEGER REFERENCES collections(id) ON DELETE CASCADE;",
            )?;
        }
        version = 6;
    }

    if version < 7 {
        // Lens name + aperture (f-number) + focal length (mm), read from EXIF/RAW
        // metadata at ingest time. Nullable: absent on unsupported cameras/lenses
        // and on every row ingested before this migration (backfill is Task 14,
        // out of scope here — pre-v7 rows simply read back NULL and are excluded
        // by an active range/lens filter, same as any other NULL metadata column).
        conn.execute_batch(
            "ALTER TABLE images ADD COLUMN lens TEXT;
             ALTER TABLE images ADD COLUMN aperture REAL;
             ALTER TABLE images ADD COLUMN focal_length REAL;",
        )?;
        version = 7;
    }

    if version < 8 {
        // Thumbnail staleness for batch edits (P7 design §5.2). A batch apply
        // writes N sidecars and flags the affected thumbnails; the virtualized
        // grid regenerates a cell when it realizes, then clears the flag.
        //
        // DEFAULT 0 (fresh) is load-bearing: upgrading an existing catalog must
        // not mark every thumbnail stale and trigger a library-wide
        // regeneration on first launch.
        //
        // A re-derivable cache column, which contract 2 explicitly permits.
        conn.execute_batch("ALTER TABLE thumbnails ADD COLUMN stale INTEGER NOT NULL DEFAULT 0;")?;
        version = 8;
    }

    debug_assert_eq!(
        version, SCHEMA_VERSION,
        "every migration block must advance `version` to SCHEMA_VERSION"
    );
    conn.pragma_update(None, "user_version", version)?;
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, rusqlite::Error> {
    let mut stmt = conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    fn table_columns(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows
    }

    #[test]
    fn migrate_creates_v3_shape() {
        let conn = Connection::open_in_memory().unwrap();
        super::migrate(&conn).unwrap();
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, super::SCHEMA_VERSION);
        assert_eq!(super::SCHEMA_VERSION, 8);

        let img = table_columns(&conn, "images");
        assert!(img.contains(&"flag".to_string()));
        assert!(img.contains(&"added_at".to_string()));
        assert!(
            img.contains(&"has_edits".to_string()),
            "has_edits column added"
        );

        let coll_cols = table_columns(&conn, "collections");
        assert!(
            coll_cols.contains(&"parent_id".to_string()),
            "parent_id column added to collections"
        );

        for t in ["tags", "image_tags", "collections", "collection_images"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {t} must exist");
        }
    }

    #[test]
    fn migrate_creates_export_queue_v5() {
        let conn = Connection::open_in_memory().unwrap();
        super::migrate(&conn).unwrap();
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('export_queue')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(cols.contains(&"image_id".to_string()));
        assert!(cols.contains(&"position".to_string()));
        assert!(cols.contains(&"added_at".to_string()));
    }

    #[test]
    fn migrate_creates_v7_lens_aperture_focal_columns() {
        let conn = Connection::open_in_memory().unwrap();
        super::migrate(&conn).unwrap();
        let cols = table_columns(&conn, "images");
        assert!(cols.contains(&"lens".to_string()), "lens column added");
        assert!(
            cols.contains(&"aperture".to_string()),
            "aperture column added"
        );
        assert!(
            cols.contains(&"focal_length".to_string()),
            "focal_length column added"
        );
    }

    #[test]
    fn migrate_creates_v8_thumbnail_stale_column_defaulting_fresh() {
        let conn = Connection::open_in_memory().unwrap();
        super::migrate(&conn).unwrap();
        let cols = table_columns(&conn, "thumbnails");
        assert!(cols.contains(&"stale".to_string()), "stale column added");

        // Existing rows must default to FRESH so upgrading an installed
        // catalog does not trigger a library-wide thumbnail regeneration.
        conn.execute("INSERT INTO folders (id, path) VALUES (1, 'p')", [])
            .ok();
        conn.execute(
            "INSERT INTO images (id, folder_id, filename, mtime, size)
             VALUES (1, 1, 'a.arw', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO thumbnails (image_id, level, w, h, format, blob)
             VALUES (1, 0, 8, 8, 'jpeg', x'00')",
            [],
        )
        .unwrap();
        let stale: i64 = conn
            .query_row("SELECT stale FROM thumbnails WHERE image_id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stale, 0, "new rows default to fresh");
    }
}
