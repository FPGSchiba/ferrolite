use rusqlite::Connection;

/// Bump this and add a `if version < N { ... }` block when the schema changes.
pub const SCHEMA_VERSION: i64 = 4;

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
                 sort_order INTEGER NOT NULL DEFAULT 0
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
        conn.execute_batch(
            "ALTER TABLE images ADD COLUMN has_edits INTEGER NOT NULL DEFAULT 0;",
        )?;
        version = 4;
    }

    debug_assert_eq!(
        version, SCHEMA_VERSION,
        "every migration block must advance `version` to SCHEMA_VERSION"
    );
    conn.pragma_update(None, "user_version", version)?;
    Ok(())
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
        assert_eq!(super::SCHEMA_VERSION, 4);

        let img = table_columns(&conn, "images");
        assert!(img.contains(&"flag".to_string()));
        assert!(img.contains(&"added_at".to_string()));
        assert!(img.contains(&"has_edits".to_string()), "has_edits column added");

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
}
