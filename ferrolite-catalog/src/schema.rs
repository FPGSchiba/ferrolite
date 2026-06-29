use rusqlite::Connection;

/// Bump this and add a `if version < N { ... }` block when the schema changes.
pub const SCHEMA_VERSION: i64 = 1;

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

    debug_assert_eq!(
        version, SCHEMA_VERSION,
        "every migration block must advance `version` to SCHEMA_VERSION"
    );
    conn.pragma_update(None, "user_version", version)?;
    Ok(())
}
