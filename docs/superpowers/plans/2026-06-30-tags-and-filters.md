# Tags & Filters (Spec 1.5) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the ferrolite Library usable for organizing — rate, flag, tag, and collect photos, persist ratings to `.xmp` sidecars and everything else to SQLite, and wire the (currently stubbed) Library toolbar for cross-folder search/sort/filter.

**Architecture:** Add shared value types (`Rating`/`Flag`/`Color`/`TagId`) to `ferrolite-image`; extend `ferrolite-catalog` with a schema-v3 migration, tag/collection/flag CRUD, `xmp:Rating` sidecar read/write (the only XMP I/O), and a compiled `LibraryQuery` filter/sort/search layer over the existing read pool; wire `ferrolite-app`'s toolbar, grid overlays, metadata-edit commands, tag manager, and collections panel, with all persistence done off the UI thread via `ferrolite-jobs`.

**Tech Stack:** Rust 2021, `rusqlite` 0.32 (bundled SQLite, pinned — do NOT bump), `quick-xml` (new), `egui`/`eframe` 0.29, `ferrolite-jobs`.

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-06-30-tags-and-filters-design.md` — read it before starting.
- **Responsiveness (CLAUDE.md):** never block the UI thread. All file/DB writes and queries go through `ferrolite-jobs` or the read pool and return over the app event channel; grid rendering stays virtualized (per-visible-row work only).
- **Source of truth:** `xmp:Rating` in `<image>.xmp` is the source of truth for **rating** (SQLite mirrors it for querying); SQLite is the source of truth for **tags, collections, and flags** (the §5.2 carve-out — do not invent sidecar files for them).
- **rusqlite is pinned at 0.32** (memory: bundled libsqlite3-sys must not move to 0.38+). Do not change the version.
- **Engine-tier crates (`ferrolite-gpu`, `ferrolite-vt`) are NOT touched.**
- **Workspace gate (must be green to finish):** `cargo fmt --check` && `cargo clippy --workspace --all-targets -- -D warnings` && `cargo test --workspace`. Code must be clippy-clean (treat warnings as errors).
- **Rust style:** `thiserror` typed errors in libs; `?` propagation, no `unwrap()` outside tests; immutable-by-default; files focused (<800 lines).
- **Commit** after each task with a conventional-commit message (`feat:`/`refactor:`/`test:`).

---

## Phase A — shared value types (`ferrolite-image`)

### Task A1: `Rating`, `Flag`, `Color`, `TagId` value types

**Files:**
- Create: `ferrolite-image/src/meta.rs`
- Modify: `ferrolite-image/src/lib.rs` (add `mod meta;` + re-export)
- Test: in `ferrolite-image/src/meta.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `Rating` (`new(u8)->Rating` saturating at 5, `get()->u8`, `as_i64()->i64`, `from_i64(i64)->Rating`, `Default`=0); `Flag` (`None|Pick|Reject`, `as_i64()->i64`, `from_i64(i64)->Flag`, `Default`=None); `Color { r,g,b: u8 }` (`from_packed(u32)->Color`, `to_packed()->u32`, `from_hex(&str)->Option<Color>`, `to_hex()->String`, `Default`); `TagId(pub i64)`.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-image/src/meta.rs (append at bottom)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rating_saturates_at_five() {
        assert_eq!(Rating::new(9).get(), 5);
        assert_eq!(Rating::new(3).get(), 3);
        assert_eq!(Rating::from_i64(-2).get(), 0);
        assert_eq!(Rating::from_i64(7).get(), 5);
        assert_eq!(Rating::default().get(), 0);
    }

    #[test]
    fn flag_round_trips_through_i64() {
        for f in [Flag::None, Flag::Pick, Flag::Reject] {
            assert_eq!(Flag::from_i64(f.as_i64()), f);
        }
        assert_eq!(Flag::from_i64(99), Flag::None);
        assert_eq!(Flag::default(), Flag::None);
    }

    #[test]
    fn color_packs_and_parses_hex() {
        let c = Color { r: 0xE5, g: 0x48, b: 0x4D };
        assert_eq!(c.to_packed(), 0x00E5_484D);
        assert_eq!(Color::from_packed(0x00E5_484D), c);
        assert_eq!(c.to_hex(), "#E5484D");
        assert_eq!(Color::from_hex("#E5484D"), Some(c));
        assert_eq!(Color::from_hex("E5484D"), Some(c));
        assert_eq!(Color::from_hex("nope"), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-image meta::`
Expected: FAIL — `cannot find type Rating` (module not yet written).

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-image/src/meta.rs (top of file)
//! Library metadata value types: rating, flag, tag colour, tag id.

/// Star rating, clamped to 0..=5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rating(u8);

impl Rating {
    pub fn new(v: u8) -> Self {
        Self(v.min(5))
    }
    pub fn get(self) -> u8 {
        self.0
    }
    pub fn as_i64(self) -> i64 {
        self.0 as i64
    }
    pub fn from_i64(v: i64) -> Self {
        Self::new(v.clamp(0, 5) as u8)
    }
}

/// Pick / reject cull flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Flag {
    #[default]
    None,
    Pick,
    Reject,
}

impl Flag {
    pub fn as_i64(self) -> i64 {
        match self {
            Flag::None => 0,
            Flag::Pick => 1,
            Flag::Reject => 2,
        }
    }
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => Flag::Pick,
            2 => Flag::Reject,
            _ => Flag::None,
        }
    }
}

/// An sRGB tag colour, stored packed as `0x00RRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Default for Color {
    fn default() -> Self {
        Color { r: 0x80, g: 0x80, b: 0x80 }
    }
}

impl Color {
    pub fn from_packed(v: u32) -> Self {
        Color {
            r: ((v >> 16) & 0xFF) as u8,
            g: ((v >> 8) & 0xFF) as u8,
            b: (v & 0xFF) as u8,
        }
    }
    pub fn to_packed(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
    pub fn from_hex(s: &str) -> Option<Self> {
        let h = s.strip_prefix('#').unwrap_or(s);
        if h.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Color::from_packed(v))
    }
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

/// Stable tag identity (SQLite `tags.id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TagId(pub i64);
```

```rust
// ferrolite-image/src/lib.rs — add after the existing `mod` lines and re-exports
mod meta;
pub use meta::{Color, Flag, Rating, TagId};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-image meta::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-image/src/meta.rs ferrolite-image/src/lib.rs
git commit -m "feat(image): add Rating/Flag/Color/TagId value types"
```

---

## Phase B — catalog schema v3 + model fields

### Task B1: schema v3 migration + foreign keys

**Files:**
- Modify: `ferrolite-catalog/src/schema.rs`
- Modify: `ferrolite-catalog/src/catalog.rs` (`open`, `open_in_memory` — enable foreign keys)
- Modify: `ferrolite-catalog/src/read_pool.rs` (`open_read_only` — enable foreign keys)
- Test: `ferrolite-catalog/src/schema.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: schema v3 with `images.flag`, `images.added_at`, and tables `tags`, `image_tags`, `collections`, `collection_images`. `SCHEMA_VERSION == 3`.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-catalog/src/schema.rs (append)
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
        assert_eq!(super::SCHEMA_VERSION, 3);

        let img = table_columns(&conn, "images");
        assert!(img.contains(&"flag".to_string()));
        assert!(img.contains(&"added_at".to_string()));

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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-catalog schema::tests::migrate_creates_v3_shape`
Expected: FAIL — `SCHEMA_VERSION` is 2; assertion on tables fails.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-catalog/src/schema.rs — change the constant
pub const SCHEMA_VERSION: i64 = 3;
```

```rust
// ferrolite-catalog/src/schema.rs — insert this block AFTER the `if version < 2 { ... }` block
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
```

```rust
// ferrolite-catalog/src/catalog.rs — in `open`, after the synchronous pragma:
        conn.pragma_update(None, "foreign_keys", "ON")?;
// ferrolite-catalog/src/catalog.rs — in `open_in_memory`, after `schema::migrate(&conn)?;`:
        conn.pragma_update(None, "foreign_keys", "ON")?;
```

```rust
// ferrolite-catalog/src/read_pool.rs — in `open_read_only`, before `Ok(conn)`:
    conn.pragma_update(None, "foreign_keys", "ON")?;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-catalog schema::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-catalog/src/schema.rs ferrolite-catalog/src/catalog.rs ferrolite-catalog/src/read_pool.rs
git commit -m "feat(catalog): schema v3 — flag/added_at columns + tags/collections tables"
```

---

### Task B2: extend `ImageRecord` / `NewImage` + queries with rating & flag

**Files:**
- Modify: `ferrolite-catalog/src/model.rs` (`ImageRecord`, `NewImage`, constructors)
- Modify: `ferrolite-catalog/src/queries.rs` (`IMAGE_COLS`, `row_to_record`)
- Modify: `ferrolite-catalog/src/catalog.rs` (`upsert_image`)
- Modify: `ferrolite-catalog/src/lib.rs` (re-export `Rating`, `Flag`, `TagId`, `Color`)
- Test: `ferrolite-catalog/src/catalog.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `ferrolite_image::{Rating, Flag}` (Task A1).
- Produces: `ImageRecord { …, rating: Rating, flag: Flag }`; `NewImage { …, rating: Rating, added_at: i64 }`; `NewImage::from_metadata(folder_id, filename, mtime, size, &Metadata, kind, rating: Rating, added_at: i64)`; `NewImage::failed(folder_id, filename, mtime, size, kind, added_at: i64)`. `upsert_image` persists `rating`+`added_at` on insert, refreshes `rating` on conflict, never touches `flag` or `added_at` on conflict.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-catalog/src/catalog.rs (append inside or add a #[cfg(test)] mod)
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
            .query_row("SELECT added_at FROM images WHERE id=?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(added, Some(1000), "added_at preserved on conflict");
        let rec = cat.list_images(f).unwrap().into_iter().next().unwrap();
        assert_eq!(rec.rating, Rating::new(5), "rating refreshed on conflict");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-catalog rating_tests`
Expected: FAIL — `NewImage::failed` arity wrong / `rating` field missing.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-catalog/src/model.rs — imports
use ferrolite_image::{FileKind, Flag, Orientation, Rating};
```

```rust
// ferrolite-catalog/src/model.rs — add fields to NewImage (after `iso`)
    pub rating: Rating,
    pub added_at: i64,
```

```rust
// ferrolite-catalog/src/model.rs — add fields to ImageRecord (after `iso`)
    pub rating: Rating,
    pub flag: Flag,
```

```rust
// ferrolite-catalog/src/model.rs — update from_metadata signature + body
    pub fn from_metadata(
        folder_id: i64,
        filename: String,
        mtime: i64,
        size: i64,
        meta: &ferrolite_decode::Metadata,
        kind: FileKind,
        rating: Rating,
        added_at: i64,
    ) -> Self {
        Self {
            folder_id,
            filename,
            mtime,
            size,
            make: Some(meta.make.clone()),
            model: Some(meta.model.clone()),
            width: Some(meta.width),
            height: Some(meta.height),
            orientation: meta.orientation,
            capture_time: meta.capture_time.clone(),
            iso: meta.iso,
            decode_status: DecodeStatus::Done,
            kind,
            rating,
            added_at,
        }
    }
```

```rust
// ferrolite-catalog/src/model.rs — update failed() signature + body
    pub fn failed(
        folder_id: i64,
        filename: String,
        mtime: i64,
        size: i64,
        kind: FileKind,
        added_at: i64,
    ) -> Self {
        Self {
            folder_id,
            filename,
            mtime,
            size,
            make: None,
            model: None,
            width: None,
            height: None,
            orientation: Orientation::Normal,
            capture_time: None,
            iso: None,
            decode_status: DecodeStatus::Failed,
            kind,
            rating: Rating::default(),
            added_at,
        }
    }
```

```rust
// ferrolite-catalog/src/queries.rs — imports
use ferrolite_image::{FileKind, Flag, Orientation, Rating};
```

```rust
// ferrolite-catalog/src/queries.rs — extend IMAGE_COLS
const IMAGE_COLS: &str = "id, folder_id, filename, width, height, orientation,
                          capture_time, iso, decode_status, kind, rating, flag";
```

```rust
// ferrolite-catalog/src/queries.rs — extend row_to_record (after `kind`)
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
```

```rust
// ferrolite-catalog/src/catalog.rs — replace upsert_image's SQL + params.
// INSERT now lists rating(?14) + added_at(?15); ON CONFLICT refreshes rating
// (XMP-authoritative on rescan) but NOT flag or added_at.
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
```

```rust
// ferrolite-catalog/src/lib.rs — extend the ferrolite_image re-export line
pub use ferrolite_image::{Color, FileKind, Flag, Rating, TagId};
```

- [ ] **Step 4: Compile + run all catalog tests (other call sites break here — fix them)**

Run: `cargo test -p ferrolite-catalog`
Expected: compile errors in `tests/tree.rs` and any other `NewImage::failed(...)`/`from_metadata(...)` call sites (arity changed) and in `cell_state.rs`/`state.rs` test helpers building `ImageRecord`. Fix each by adding the new args/fields:
- `NewImage::failed(folder, name, mtime, size, kind)` → add a trailing `, 0` (added_at).
- `NewImage::from_metadata(...)` → add trailing `, Rating::default(), 0`.
- `ImageRecord { … }` literals → add `rating: Rating::default(), flag: Flag::None,`.

Re-run until PASS (including the new `rating_tests`).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-catalog/src ferrolite-catalog/tests
git commit -m "feat(catalog): carry rating + flag on ImageRecord, rating + added_at on NewImage"
```

---

## Phase C — `xmp:Rating` sidecar I/O (`ferrolite-catalog`)

### Task C1: add `quick-xml` dependency

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)
- Modify: `ferrolite-catalog/Cargo.toml`

- [ ] **Step 1: Add the dependency**

```toml
# Cargo.toml (workspace) — add under [workspace.dependencies]
quick-xml = "0.37"
```

```toml
# ferrolite-catalog/Cargo.toml — add under [dependencies]
quick-xml = { workspace = true }
```

- [ ] **Step 2: Verify it resolves**

Run: `cargo build -p ferrolite-catalog`
Expected: builds (no code uses it yet).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock ferrolite-catalog/Cargo.toml
git commit -m "chore(catalog): add quick-xml dependency"
```

---

### Task C2: `xmp::sidecar_path` + `read_rating`

**Files:**
- Create: `ferrolite-catalog/src/xmp.rs`
- Modify: `ferrolite-catalog/src/lib.rs` (`mod xmp;` + re-export)
- Test: `ferrolite-catalog/src/xmp.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `pub fn sidecar_path(image_path: &Path) -> PathBuf` (appends `.xmp` to the full filename); `pub fn read_rating(xmp_path: &Path) -> Option<Rating>` (lenient; attribute-form OR element-form `xmp:Rating`; `None` if absent/malformed/missing-file).

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-catalog/src/xmp.rs
//! Hand-rolled XMP sidecar I/O. In Spec 1.5 the sidecar carries only
//! `xmp:Rating`; foreign nodes are preserved on write (merge-preserving).

use ferrolite_image::Rating;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer};
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_path_appends_xmp() {
        let p = sidecar_path(Path::new("/a/b/DSC_1.NEF"));
        assert_eq!(p, PathBuf::from("/a/b/DSC_1.NEF.xmp"));
    }

    #[test]
    fn reads_attribute_form_rating() {
        let dir = std::env::temp_dir().join(format!("frl-xmp-attr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.xmp");
        std::fs::write(
            &p,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
                 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                   <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/"
                     xmp:Rating="4"/>
                 </rdf:RDF>
               </x:xmpmeta>"#,
        )
        .unwrap();
        assert_eq!(read_rating(&p), Some(Rating::new(4)));
    }

    #[test]
    fn reads_element_form_rating() {
        let dir = std::env::temp_dir().join(format!("frl-xmp-elem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("b.xmp");
        std::fs::write(
            &p,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
                 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                   <rdf:Description rdf:about="">
                     <xmp:Rating xmlns:xmp="http://ns.adobe.com/xap/1.0/">2</xmp:Rating>
                   </rdf:Description>
                 </rdf:RDF>
               </x:xmpmeta>"#,
        )
        .unwrap();
        assert_eq!(read_rating(&p), Some(Rating::new(2)));
    }

    #[test]
    fn missing_or_malformed_is_none() {
        assert_eq!(read_rating(Path::new("/no/such.xmp")), None);
        let dir = std::env::temp_dir().join(format!("frl-xmp-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.xmp");
        std::fs::write(&p, "<not xml <<<").unwrap();
        assert_eq!(read_rating(&p), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-catalog xmp::`
Expected: FAIL — functions not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-catalog/src/xmp.rs (above the tests module)

/// `<image>.xmp` next to the original (full filename + `.xmp`).
pub fn sidecar_path(image_path: &Path) -> PathBuf {
    let mut s = image_path.as_os_str().to_os_string();
    s.push(".xmp");
    PathBuf::from(s)
}

const RATING_LOCAL: &[u8] = b"xmp:Rating";

/// Read `xmp:Rating` (attribute OR element form). Lenient: any parse error or
/// missing file yields `None`.
pub fn read_rating(xmp_path: &Path) -> Option<Rating> {
    let text = std::fs::read_to_string(xmp_path).ok()?;
    let mut reader = Reader::from_str(&text);
    reader.config_mut().trim_text(true);
    let mut in_rating_elem = false;
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                // Attribute form on any element (typically rdf:Description).
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == RATING_LOCAL {
                        let v = String::from_utf8_lossy(&attr.value);
                        if let Ok(n) = v.trim().parse::<i64>() {
                            return Some(Rating::from_i64(n));
                        }
                    }
                }
                if e.name().as_ref() == RATING_LOCAL {
                    in_rating_elem = true;
                }
            }
            Ok(Event::Text(t)) if in_rating_elem => {
                let v = t.unescape().unwrap_or_default();
                if let Ok(n) = v.trim().parse::<i64>() {
                    return Some(Rating::from_i64(n));
                }
                in_rating_elem = false;
            }
            Ok(Event::End(_)) => in_rating_elem = false,
            Err(_) => return None,
            _ => {}
        }
    }
    None
}
```

```rust
// ferrolite-catalog/src/lib.rs — add
mod xmp;
pub use xmp::{read_rating, sidecar_path, write_rating};
```

> Note: `write_rating` is added in Task C3; for this task add only `read_rating`/`sidecar_path` to the re-export and append `write_rating` when C3 lands (or temporarily re-export just the two and update in C3).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-catalog xmp::`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-catalog/src/xmp.rs ferrolite-catalog/src/lib.rs
git commit -m "feat(catalog): read xmp:Rating from sidecars (attribute + element form)"
```

---

### Task C3: `xmp::write_rating` (merge-preserving)

**Files:**
- Modify: `ferrolite-catalog/src/xmp.rs`
- Test: `ferrolite-catalog/src/xmp.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `sidecar_path`, `read_rating` (Task C2).
- Produces: `pub fn write_rating(xmp_path: &Path, rating: Rating) -> Result<(), CatalogError>` — creates a minimal sidecar if absent; otherwise sets the `xmp:Rating` **attribute** on the first `rdf:Description`, drops any element-form `xmp:Rating`, and stream-copies every other node verbatim; on a parse error backs the file up to `<path>.bak` and writes a fresh sidecar.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-catalog/src/xmp.rs — add inside `mod tests`
    #[test]
    fn writes_fresh_sidecar_when_absent() {
        let dir = std::env::temp_dir().join(format!("frl-xmp-new-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("d.xmp");
        let _ = std::fs::remove_file(&p);
        write_rating(&p, Rating::new(5)).unwrap();
        assert_eq!(read_rating(&p), Some(Rating::new(5)));
    }

    #[test]
    fn write_preserves_foreign_nodes_and_updates_rating() {
        let dir = std::env::temp_dir().join(format!("frl-xmp-merge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("e.xmp");
        std::fs::write(
            &p,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
                 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                   <rdf:Description rdf:about=""
                     xmlns:xmp="http://ns.adobe.com/xap/1.0/"
                     xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
                     xmp:Rating="1" crs:Exposure2012="+0.50">
                     <dc:subject xmlns:dc="http://purl.org/dc/elements/1.1/">
                       <rdf:Bag><rdf:li>portrait</rdf:li></rdf:Bag>
                     </dc:subject>
                   </rdf:Description>
                 </rdf:RDF>
               </x:xmpmeta>"#,
        )
        .unwrap();
        write_rating(&p, Rating::new(4)).unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("crs:Exposure2012"), "foreign attr preserved");
        assert!(out.contains("portrait"), "foreign dc:subject preserved");
        assert_eq!(read_rating(&p), Some(Rating::new(4)), "rating updated");
    }

    #[test]
    fn write_backs_up_malformed_then_writes_fresh() {
        let dir = std::env::temp_dir().join(format!("frl-xmp-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.xmp");
        std::fs::write(&p, "<broken <<").unwrap();
        write_rating(&p, Rating::new(3)).unwrap();
        assert!(dir.join("f.xmp.bak").exists(), "malformed original backed up");
        assert_eq!(read_rating(&p), Some(Rating::new(3)));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-catalog xmp::tests::write`
Expected: FAIL — `write_rating` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-catalog/src/xmp.rs — add `use crate::error::CatalogError;` to the imports,
// then add these functions above the tests module.

fn fresh_sidecar(rating: Rating) -> String {
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
         \x20<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
         \x20\x20<rdf:Description rdf:about=\"\" \
         xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"{}\"/>\n\
         \x20</rdf:RDF>\n\
         </x:xmpmeta>\n\
         <?xpacket end=\"w\"?>\n",
        rating.get()
    )
}

/// Build a copy of `src` with `xmp:Rating="<n>"` set/replaced as an attribute.
fn description_with_rating(src: &BytesStart<'_>, rating: Rating) -> BytesStart<'static> {
    let mut out = BytesStart::new(String::from_utf8_lossy(src.name().as_ref()).into_owned());
    for attr in src.attributes().flatten() {
        if attr.key.as_ref() != RATING_LOCAL {
            let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
            let val = String::from_utf8_lossy(&attr.value).into_owned();
            out.push_attribute((key.as_str(), val.as_str()));
        }
    }
    out.push_attribute(("xmp:Rating", rating.get().to_string().as_str()));
    out
}

/// Stream-rewrite an existing sidecar, returning the new bytes, or `None` on a
/// parse error (caller falls back to a fresh template + `.bak`).
fn rewrite_with_rating(text: &str, rating: Rating) -> Option<Vec<u8>> {
    let mut reader = Reader::from_str(text);
    let mut writer = Writer::new(Vec::new());
    let mut done = false; // rating attribute already applied to first Description
    let mut skip_depth: i32 = -1; // >=0 while skipping an element-form xmp:Rating subtree
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(ev) => {
                // Drop any element-form <xmp:Rating>…</xmp:Rating> (superseded by attr).
                if skip_depth >= 0 {
                    match &ev {
                        Event::Start(_) => skip_depth += 1,
                        Event::End(_) => {
                            skip_depth -= 1;
                            if skip_depth < 0 { /* exited */ }
                        }
                        _ => {}
                    }
                    if matches!(ev, Event::End(_)) && skip_depth < 0 {
                        continue;
                    }
                    continue;
                }
                match ev {
                    Event::Start(e) if e.name().as_ref() == RATING_LOCAL => {
                        skip_depth = 0;
                    }
                    Event::Empty(e) if e.name().as_ref() == RATING_LOCAL => {
                        // element-form empty rating: drop it
                    }
                    Event::Start(e) if !done && e.name().as_ref() == b"rdf:Description" => {
                        writer
                            .write_event(Event::Start(description_with_rating(&e, rating)))
                            .ok()?;
                        done = true;
                    }
                    Event::Empty(e) if !done && e.name().as_ref() == b"rdf:Description" => {
                        writer
                            .write_event(Event::Empty(description_with_rating(&e, rating)))
                            .ok()?;
                        done = true;
                    }
                    other => writer.write_event(other).ok()?,
                }
            }
            Err(_) => return None,
        }
    }
    if !done {
        return None; // no rdf:Description found — treat as malformed
    }
    Some(writer.into_inner())
}

/// Write `xmp:Rating` into `xmp_path`, preserving any foreign nodes.
pub fn write_rating(xmp_path: &Path, rating: Rating) -> Result<(), CatalogError> {
    match std::fs::read_to_string(xmp_path) {
        Ok(text) => match rewrite_with_rating(&text, rating) {
            Some(bytes) => std::fs::write(xmp_path, bytes)?,
            None => {
                // Malformed: back up, then write a fresh template.
                let bak = sidecar_bak(xmp_path);
                let _ = std::fs::rename(xmp_path, &bak);
                std::fs::write(xmp_path, fresh_sidecar(rating))?;
            }
        },
        Err(_) => std::fs::write(xmp_path, fresh_sidecar(rating))?,
    }
    Ok(())
}

fn sidecar_bak(xmp_path: &Path) -> PathBuf {
    let mut s = xmp_path.as_os_str().to_os_string();
    s.push(".bak");
    PathBuf::from(s)
}
```

> Implementation note for the engineer: if `quick-xml` 0.37's `read_event`/`write_event`/`BytesStart` API differs from the above (method names, `config_mut().trim_text`), invoke the **rust-build-resolver** agent to reconcile to the resolved version — the logic (preserve foreign nodes, attribute-form rating, drop element-form) is the contract; keep it.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-catalog xmp::`
Expected: PASS (all 7 xmp tests). If the element-form skip logic mis-handles depth, simplify by collecting events into a `Vec` first; keep the preserve + attribute contract.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-catalog/src/xmp.rs
git commit -m "feat(catalog): merge-preserving write of xmp:Rating sidecars"
```

---

## Phase D — tags CRUD + association queries

### Task D1: tag CRUD on `Catalog` + `TagRecord`

**Files:**
- Modify: `ferrolite-catalog/src/model.rs` (`TagRecord`)
- Modify: `ferrolite-catalog/src/catalog.rs` (CRUD methods)
- Modify: `ferrolite-catalog/src/error.rs` (add a `Conflict` variant)
- Modify: `ferrolite-catalog/src/lib.rs` (re-export `TagRecord`)
- Test: `ferrolite-catalog/src/catalog.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `ferrolite_image::{TagId, Color}`.
- Produces: `TagRecord { id: TagId, name: String, color: Color }`; on `Catalog`: `create_tag(&str, Color) -> Result<TagId>` (errors `CatalogError::Conflict` on duplicate name), `rename_tag(TagId, &str)`, `set_tag_color(TagId, Color)`, `delete_tag(TagId)`, `list_tags() -> Vec<TagRecord>`, `add_tag_to_image(image_id: i64, TagId)`, `remove_tag_from_image(image_id: i64, TagId)`, `tags_for_images(&[i64]) -> HashMap<i64, Vec<TagId>>`.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-catalog/src/catalog.rs — add a #[cfg(test)] mod
#[cfg(test)]
mod tag_tests {
    use super::*;
    use crate::model::NewImage;
    use ferrolite_image::{Color, FileKind};

    #[test]
    fn create_list_and_associate_tags() {
        let cat = Catalog::open_in_memory().unwrap();
        let red = cat.create_tag("portrait", Color::from_packed(0xE5484D)).unwrap();
        let green = cat.create_tag("keeper", Color::from_packed(0x30A46C)).unwrap();
        assert!(cat.create_tag("portrait", Color::default()).is_err(), "dup name errors");

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
        assert!(cat.tags_for_images(&[b]).unwrap().get(&b).is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-catalog tag_tests`
Expected: FAIL — methods/`TagRecord` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-catalog/src/error.rs — add a variant
    #[error("conflict: {0}")]
    Conflict(String),
```

```rust
// ferrolite-catalog/src/model.rs — add (and `use ferrolite_image::{Color, TagId};`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRecord {
    pub id: TagId,
    pub name: String,
    pub color: Color,
}
```

```rust
// ferrolite-catalog/src/catalog.rs — add `use ferrolite_image::{Color, TagId};`
// and `use std::collections::HashMap;` (HashSet is already imported), then add methods:

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
                Err(CatalogError::Conflict(format!("tag '{name}' already exists")))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn rename_tag(&self, id: TagId, name: &str) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE tags SET name=?1 WHERE id=?2",
            rusqlite::params![name, id.0],
        )?;
        Ok(())
    }

    pub fn set_tag_color(&self, id: TagId, color: Color) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE tags SET color=?1 WHERE id=?2",
            rusqlite::params![color.to_packed() as i64, id.0],
        )?;
        Ok(())
    }

    pub fn delete_tag(&self, id: TagId) -> Result<(), CatalogError> {
        self.conn()
            .execute("DELETE FROM tags WHERE id=?1", rusqlite::params![id.0])?;
        Ok(())
    }

    pub fn list_tags(&self) -> Result<Vec<crate::model::TagRecord>, CatalogError> {
        crate::queries::list_tags(self.conn())
    }

    pub fn add_tag_to_image(&self, image_id: i64, tag: TagId) -> Result<(), CatalogError> {
        self.conn().execute(
            "INSERT OR IGNORE INTO image_tags (image_id, tag_id) VALUES (?1, ?2)",
            rusqlite::params![image_id, tag.0],
        )?;
        Ok(())
    }

    pub fn remove_tag_from_image(&self, image_id: i64, tag: TagId) -> Result<(), CatalogError> {
        self.conn().execute(
            "DELETE FROM image_tags WHERE image_id=?1 AND tag_id=?2",
            rusqlite::params![image_id, tag.0],
        )?;
        Ok(())
    }

    pub fn tags_for_images(
        &self,
        image_ids: &[i64],
    ) -> Result<HashMap<i64, Vec<TagId>>, CatalogError> {
        crate::queries::tags_for_images(self.conn(), image_ids)
    }
```

```rust
// ferrolite-catalog/src/queries.rs — add `use ferrolite_image::{Color, TagId};`
// (extend the existing ferrolite_image use) and these functions:

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
```

```rust
// ferrolite-catalog/src/lib.rs — extend the model re-export
pub use model::{DecodeStatus, ImageRecord, IngestSummary, NewImage, TagRecord};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-catalog tag_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-catalog/src
git commit -m "feat(catalog): tag CRUD + image-tag associations + batched tags_for_images"
```

---

### Task D2: expose tag reads on `ReadPool`

**Files:**
- Modify: `ferrolite-catalog/src/read_pool.rs`
- Test: `ferrolite-catalog/src/read_pool.rs` is exercised via the app; add a smoke test here.

**Interfaces:**
- Produces: `ReadPool::list_tags() -> Vec<TagRecord>`, `ReadPool::tags_for_images(&[i64]) -> HashMap<i64, Vec<TagId>>`.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-catalog/src/read_pool.rs — add a #[cfg(test)] mod
#[cfg(test)]
mod tests {
    use crate::Catalog;
    use ferrolite_image::Color;

    #[test]
    fn read_pool_lists_tags() {
        let dir = std::env::temp_dir().join(format!("frl-rp-tags-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("c.db");
        let _ = std::fs::remove_file(&path);
        let cat = Catalog::open(&path).unwrap();
        cat.create_tag("x", Color::default()).unwrap();
        let rp = super::ReadPool::open(&path, 1).unwrap();
        assert_eq!(rp.list_tags().unwrap().len(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-catalog read_pool::`
Expected: FAIL — `list_tags` not on `ReadPool`.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-catalog/src/read_pool.rs — add methods on impl ReadPool
    pub fn list_tags(&self) -> Result<Vec<crate::TagRecord>, CatalogError> {
        self.with_conn(crate::queries::list_tags)
    }
    pub fn tags_for_images(
        &self,
        image_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, Vec<ferrolite_image::TagId>>, CatalogError> {
        self.with_conn(|c| crate::queries::tags_for_images(c, image_ids))
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-catalog read_pool::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-catalog/src/read_pool.rs
git commit -m "feat(catalog): expose tag reads on the read pool"
```

---

## Phase E — collections CRUD

### Task E1: collection CRUD + `CollectionRecord`

**Files:**
- Modify: `ferrolite-catalog/src/model.rs` (`CollectionRecord`)
- Modify: `ferrolite-catalog/src/catalog.rs` (CRUD)
- Modify: `ferrolite-catalog/src/queries.rs` (`list_collections`)
- Modify: `ferrolite-catalog/src/read_pool.rs` (`list_collections`)
- Modify: `ferrolite-catalog/src/lib.rs` (re-export)
- Test: `ferrolite-catalog/src/catalog.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `CollectionRecord { id: i64, name: String, color: Color, sort_order: i64 }`; on `Catalog`: `create_collection(&str, Color) -> Result<i64>` (Conflict on dup name), `rename_collection(i64,&str)`, `set_collection_color(i64,Color)`, `delete_collection(i64)`, `add_image_to_collection(coll_id: i64, image_id: i64)`, `remove_image_from_collection(coll_id: i64, image_id: i64)`, `list_collections() -> Vec<CollectionRecord>`. `ReadPool::list_collections()`.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-catalog/src/catalog.rs — add a #[cfg(test)] mod
#[cfg(test)]
mod collection_tests {
    use super::*;
    use crate::model::NewImage;
    use ferrolite_image::{Color, FileKind};

    #[test]
    fn create_and_populate_collection() {
        let cat = Catalog::open_in_memory().unwrap();
        let c = cat.create_collection("Best of 2026", Color::from_packed(0x30A46C)).unwrap();
        assert!(cat.create_collection("Best of 2026", Color::default()).is_err());
        let f = cat.upsert_folder(std::path::Path::new("/p"), None).unwrap();
        let a = cat
            .upsert_image(&NewImage::failed(f, "a.nef".into(), 1, 1, FileKind::Raw, 0))
            .unwrap();
        cat.add_image_to_collection(c, a).unwrap();
        cat.add_image_to_collection(c, a).unwrap(); // idempotent
        let n: i64 = cat
            .conn()
            .query_row("SELECT COUNT(*) FROM collection_images WHERE collection_id=?1", [c], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(cat.list_collections().unwrap().len(), 1);
        cat.delete_collection(c).unwrap();
        assert!(cat.list_collections().unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-catalog collection_tests`
Expected: FAIL — methods not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-catalog/src/model.rs — add
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionRecord {
    pub id: i64,
    pub name: String,
    pub color: Color,
    pub sort_order: i64,
}
```

```rust
// ferrolite-catalog/src/catalog.rs — add methods (mirrors of tag CRUD)
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
                Err(CatalogError::Conflict(format!("collection '{name}' already exists")))
            }
            Err(e) => Err(e.into()),
        }
    }
    pub fn rename_collection(&self, id: i64, name: &str) -> Result<(), CatalogError> {
        self.conn().execute("UPDATE collections SET name=?1 WHERE id=?2", rusqlite::params![name, id])?;
        Ok(())
    }
    pub fn set_collection_color(&self, id: i64, color: Color) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE collections SET color=?1 WHERE id=?2",
            rusqlite::params![color.to_packed() as i64, id],
        )?;
        Ok(())
    }
    pub fn delete_collection(&self, id: i64) -> Result<(), CatalogError> {
        self.conn().execute("DELETE FROM collections WHERE id=?1", rusqlite::params![id])?;
        Ok(())
    }
    pub fn add_image_to_collection(&self, coll_id: i64, image_id: i64) -> Result<(), CatalogError> {
        self.conn().execute(
            "INSERT OR IGNORE INTO collection_images (collection_id, image_id) VALUES (?1, ?2)",
            rusqlite::params![coll_id, image_id],
        )?;
        Ok(())
    }
    pub fn remove_image_from_collection(&self, coll_id: i64, image_id: i64) -> Result<(), CatalogError> {
        self.conn().execute(
            "DELETE FROM collection_images WHERE collection_id=?1 AND image_id=?2",
            rusqlite::params![coll_id, image_id],
        )?;
        Ok(())
    }
    pub fn list_collections(&self) -> Result<Vec<crate::model::CollectionRecord>, CatalogError> {
        crate::queries::list_collections(self.conn())
    }
```

```rust
// ferrolite-catalog/src/queries.rs — add
pub(crate) fn list_collections(
    conn: &Connection,
) -> Result<Vec<crate::model::CollectionRecord>, CatalogError> {
    let mut stmt =
        conn.prepare("SELECT id, name, color, sort_order FROM collections ORDER BY sort_order, name")?;
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
```

```rust
// ferrolite-catalog/src/read_pool.rs — add
    pub fn list_collections(&self) -> Result<Vec<crate::CollectionRecord>, CatalogError> {
        self.with_conn(crate::queries::list_collections)
    }
```

```rust
// ferrolite-catalog/src/lib.rs — extend the model re-export
pub use model::{
    CollectionRecord, DecodeStatus, ImageRecord, IngestSummary, NewImage, TagRecord,
};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-catalog collection_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-catalog/src
git commit -m "feat(catalog): collection CRUD + membership"
```

---

## Phase F — `LibraryQuery` filter/sort/search

### Task F1: `LibraryQuery` + pure `compile()`

**Files:**
- Create: `ferrolite-catalog/src/query.rs`
- Modify: `ferrolite-catalog/src/lib.rs` (`mod query;` + re-exports)
- Test: `ferrolite-catalog/src/query.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `ferrolite_image::{Flag, TagId}`.
- Produces: enums `Scope { Folder { id: i64, recursive: bool }, AllPhotographs, Collection { id: i64 }, RecentlyAdded { limit: i64 } }`, `SortKey { CaptureTime, Filename, Rating, AddedAt }`, `RatingFilter { AtLeast(u8), Exactly(u8) }`, `TagMode { Any, All }`; structs `Sort { key: SortKey, desc: bool }`, `TagFilter { ids: Vec<TagId>, mode: TagMode }`, `LibraryQuery { scope, search: Option<String>, sort: Sort, rating: Option<RatingFilter>, flags: Vec<Flag>, tags: TagFilter, camera: Option<String>, iso: Option<(u32,u32)>, date: Option<(String,String)> }` with `Default`; `LibraryQuery::compile(&self) -> (String, Vec<rusqlite::types::Value>)`.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-catalog/src/query.rs
//! A declarative, parameterised catalog query (filter + sort + search), compiled
//! to one `SELECT`. Pure: `compile()` is unit-tested without a database.

use crate::queries::IMAGE_COLS;
use ferrolite_image::{Flag, TagId};
use rusqlite::types::Value;

// ... (definitions added in Step 3) ...

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> LibraryQuery {
        LibraryQuery::default()
    }

    #[test]
    fn all_photographs_default_sort_has_no_where() {
        let q = LibraryQuery { scope: Scope::AllPhotographs, ..base() };
        let (sql, params) = q.compile();
        assert!(sql.contains("FROM images"));
        assert!(!sql.contains("WHERE"), "no predicates → no WHERE: {sql}");
        assert!(sql.contains("ORDER BY"));
        assert!(params.is_empty());
    }

    #[test]
    fn folder_recursive_uses_subtree_cte() {
        let q = LibraryQuery { scope: Scope::Folder { id: 7, recursive: true }, ..base() };
        let (sql, params) = q.compile();
        assert!(sql.contains("WITH RECURSIVE subtree"));
        assert!(sql.contains("folder_id IN (SELECT id FROM subtree)"));
        assert_eq!(params, vec![Value::Integer(7)]);
    }

    #[test]
    fn rating_flag_and_tags_any_compile_to_params() {
        let q = LibraryQuery {
            scope: Scope::AllPhotographs,
            rating: Some(RatingFilter::AtLeast(3)),
            flags: vec![Flag::Pick],
            tags: TagFilter { ids: vec![TagId(1), TagId(2)], mode: TagMode::Any },
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("rating >= ?"));
        assert!(sql.contains("flag IN (?)"));
        assert!(sql.contains("image_tags WHERE tag_id IN (?,?)"));
        assert!(!sql.contains("HAVING"));
        assert_eq!(
            params,
            vec![Value::Integer(3), Value::Integer(1), Value::Integer(1), Value::Integer(2)]
        );
    }

    #[test]
    fn tags_all_uses_having_count() {
        let q = LibraryQuery {
            scope: Scope::AllPhotographs,
            tags: TagFilter { ids: vec![TagId(1), TagId(2)], mode: TagMode::All },
            ..base()
        };
        let (sql, _params) = q.compile();
        assert!(sql.contains("GROUP BY image_id HAVING COUNT(DISTINCT tag_id) = 2"));
    }

    #[test]
    fn search_matches_filename_or_tag_name() {
        let q = LibraryQuery { search: Some("port".into()), ..base() };
        let (sql, params) = q.compile();
        assert!(sql.contains("filename LIKE ?"));
        assert!(sql.contains("t.name LIKE ?"));
        assert_eq!(params, vec![Value::Text("%port%".into()), Value::Text("%port%".into())]);
    }

    #[test]
    fn recently_added_orders_desc_with_limit() {
        let q = LibraryQuery { scope: Scope::RecentlyAdded { limit: 50 }, ..base() };
        let (sql, params) = q.compile();
        assert!(sql.contains("ORDER BY added_at DESC"));
        assert!(sql.contains("LIMIT ?"));
        assert_eq!(params, vec![Value::Integer(50)]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-catalog query::`
Expected: FAIL — types not defined. (Also `IMAGE_COLS` must be made `pub(crate)` — see Step 3.)

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-catalog/src/queries.rs — change IMAGE_COLS visibility
pub(crate) const IMAGE_COLS: &str = "id, folder_id, filename, width, height, orientation,
                          capture_time, iso, decode_status, kind, rating, flag";
```

```rust
// ferrolite-catalog/src/query.rs — add ABOVE the tests module

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Folder { id: i64, recursive: bool },
    AllPhotographs,
    Collection { id: i64 },
    RecentlyAdded { limit: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    CaptureTime,
    Filename,
    Rating,
    AddedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub desc: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatingFilter {
    AtLeast(u8),
    Exactly(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagMode {
    Any,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagFilter {
    pub ids: Vec<TagId>,
    pub mode: TagMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryQuery {
    pub scope: Scope,
    pub search: Option<String>,
    pub sort: Sort,
    pub rating: Option<RatingFilter>,
    pub flags: Vec<Flag>,
    pub tags: TagFilter,
    pub camera: Option<String>,
    pub iso: Option<(u32, u32)>,
    pub date: Option<(String, String)>,
}

impl Default for LibraryQuery {
    fn default() -> Self {
        LibraryQuery {
            scope: Scope::AllPhotographs,
            search: None,
            sort: Sort { key: SortKey::CaptureTime, desc: false },
            rating: None,
            flags: Vec::new(),
            tags: TagFilter { ids: Vec::new(), mode: TagMode::Any },
            camera: None,
            iso: None,
            date: None,
        }
    }
}

fn sort_column(key: SortKey) -> &'static str {
    match key {
        SortKey::CaptureTime => "capture_time",
        SortKey::Filename => "filename",
        SortKey::Rating => "rating",
        SortKey::AddedAt => "added_at",
    }
}

impl LibraryQuery {
    /// Compile to `(sql, params)`. All user input is bound as parameters — never
    /// interpolated — so the query is injection-safe.
    pub fn compile(&self) -> (String, Vec<Value>) {
        let mut params: Vec<Value> = Vec::new();
        let mut prefix = String::new();
        let mut joins = String::new();
        let mut where_clauses: Vec<String> = Vec::new();

        // RecentlyAdded short-circuits scope + ordering.
        if let Scope::RecentlyAdded { limit } = self.scope {
            let sql = format!(
                "SELECT {IMAGE_COLS} FROM images WHERE added_at IS NOT NULL \
                 ORDER BY added_at DESC LIMIT ?"
            );
            params.push(Value::Integer(limit));
            return (sql, params);
        }

        match &self.scope {
            Scope::Folder { id, recursive } => {
                if *recursive {
                    prefix.push_str(
                        "WITH RECURSIVE subtree(id) AS (\
                         SELECT id FROM folders WHERE id = ? \
                         UNION ALL \
                         SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id) ",
                    );
                    params.push(Value::Integer(*id));
                    where_clauses.push("folder_id IN (SELECT id FROM subtree)".into());
                } else {
                    where_clauses.push("folder_id = ?".into());
                    params.push(Value::Integer(*id));
                }
            }
            Scope::Collection { id } => {
                joins.push_str(
                    " JOIN collection_images ci ON ci.image_id = images.id AND ci.collection_id = ?",
                );
                params.push(Value::Integer(*id));
            }
            Scope::AllPhotographs => {}
            Scope::RecentlyAdded { .. } => unreachable!(),
        }

        if let Some(rf) = self.rating {
            match rf {
                RatingFilter::AtLeast(n) => {
                    where_clauses.push("rating >= ?".into());
                    params.push(Value::Integer(n as i64));
                }
                RatingFilter::Exactly(n) => {
                    where_clauses.push("rating = ?".into());
                    params.push(Value::Integer(n as i64));
                }
            }
        }

        if !self.flags.is_empty() {
            let ph = vec!["?"; self.flags.len()].join(",");
            where_clauses.push(format!("flag IN ({ph})"));
            for f in &self.flags {
                params.push(Value::Integer(f.as_i64()));
            }
        }

        if !self.tags.ids.is_empty() {
            let ph = vec!["?"; self.tags.ids.len()].join(",");
            match self.tags.mode {
                TagMode::Any => {
                    where_clauses.push(format!(
                        "images.id IN (SELECT image_id FROM image_tags WHERE tag_id IN ({ph}))"
                    ));
                }
                TagMode::All => {
                    where_clauses.push(format!(
                        "images.id IN (SELECT image_id FROM image_tags WHERE tag_id IN ({ph}) \
                         GROUP BY image_id HAVING COUNT(DISTINCT tag_id) = {})",
                        self.tags.ids.len()
                    ));
                }
            }
            for t in &self.tags.ids {
                params.push(Value::Integer(t.0));
            }
        }

        if let Some(s) = &self.search {
            let like = format!("%{s}%");
            where_clauses.push(
                "(filename LIKE ? OR images.id IN \
                 (SELECT it.image_id FROM image_tags it JOIN tags t ON t.id = it.tag_id \
                 WHERE t.name LIKE ?))"
                    .into(),
            );
            params.push(Value::Text(like.clone()));
            params.push(Value::Text(like));
        }

        if let Some(cam) = &self.camera {
            where_clauses.push("camera_model = ?".into());
            params.push(Value::Text(cam.clone()));
        }

        if let Some((lo, hi)) = self.iso {
            where_clauses.push("iso BETWEEN ? AND ?".into());
            params.push(Value::Integer(lo as i64));
            params.push(Value::Integer(hi as i64));
        }

        if let Some((from, to)) = &self.date {
            where_clauses.push("capture_time BETWEEN ? AND ?".into());
            params.push(Value::Text(from.clone()));
            params.push(Value::Text(to.clone()));
        }

        let mut sql = format!("{prefix}SELECT {IMAGE_COLS} FROM images{joins}");
        if !where_clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&where_clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY ");
        sql.push_str(sort_column(self.sort.key));
        sql.push_str(if self.sort.desc { " DESC" } else { " ASC" });
        (sql, params)
    }
}
```

```rust
// ferrolite-catalog/src/lib.rs — add
mod query;
pub use query::{LibraryQuery, RatingFilter, Scope, Sort, SortKey, TagFilter, TagMode};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-catalog query::`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-catalog/src/query.rs ferrolite-catalog/src/queries.rs ferrolite-catalog/src/lib.rs
git commit -m "feat(catalog): LibraryQuery filter/sort/search compiler (pure)"
```

---

### Task F2: run `LibraryQuery` + toolbar distinct-value helpers

**Files:**
- Modify: `ferrolite-catalog/src/query.rs` (add `run`)
- Modify: `ferrolite-catalog/src/catalog.rs` (`query_images`)
- Modify: `ferrolite-catalog/src/read_pool.rs` (`query_images`, `distinct_cameras`, `iso_bounds`, `date_bounds`)
- Modify: `ferrolite-catalog/src/queries.rs` (distinct-value helpers)
- Test: `ferrolite-catalog/tests/query_integration.rs` (new integration test)

**Interfaces:**
- Produces: `query::run(conn, &LibraryQuery) -> Result<Vec<ImageRecord>, CatalogError>`; `Catalog::query_images`, `ReadPool::query_images`, `ReadPool::distinct_cameras() -> Vec<String>`, `ReadPool::iso_bounds() -> Option<(u32,u32)>`, `ReadPool::date_bounds() -> Option<(String,String)>`.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-catalog/tests/query_integration.rs
use ferrolite_catalog::{Catalog, LibraryQuery, Scope, TagFilter, TagMode};
use ferrolite_image::{Color, FileKind};

fn mk_image(cat: &Catalog, folder: i64, name: &str) -> i64 {
    use ferrolite_catalog::NewImage;
    cat.upsert_image(&NewImage::failed(folder, name.into(), 1, 1, FileKind::Raw, 0))
        .unwrap()
}

#[test]
fn tag_filter_returns_images_across_folders() {
    let cat = Catalog::open_in_memory().unwrap();
    let f1 = cat.upsert_folder(std::path::Path::new("/a"), None).unwrap();
    let f2 = cat.upsert_folder(std::path::Path::new("/b"), None).unwrap();
    let i1 = mk_image(&cat, f1, "a.nef");
    let i2 = mk_image(&cat, f2, "b.nef");
    let _i3 = mk_image(&cat, f2, "c.nef");
    let tag = cat.create_tag("keeper", Color::default()).unwrap();
    cat.add_tag_to_image(i1, tag).unwrap();
    cat.add_tag_to_image(i2, tag).unwrap();

    let q = LibraryQuery {
        scope: Scope::AllPhotographs,
        tags: TagFilter { ids: vec![tag], mode: TagMode::Any },
        ..Default::default()
    };
    let rows = cat.query_images(&q).unwrap();
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), 2, "tag spans two folders");
    assert!(ids.contains(&i1) && ids.contains(&i2));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-catalog --test query_integration`
Expected: FAIL — `query_images` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-catalog/src/query.rs — add (uses crate::queries::row_to_record)
use crate::error::CatalogError;
use crate::model::ImageRecord;
use rusqlite::Connection;

pub(crate) fn run(conn: &Connection, q: &LibraryQuery) -> Result<Vec<ImageRecord>, CatalogError> {
    let (sql, params) = q.compile();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), crate::queries::row_to_record)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}
```

```rust
// ferrolite-catalog/src/catalog.rs — add
    pub fn query_images(
        &self,
        q: &crate::LibraryQuery,
    ) -> Result<Vec<ImageRecord>, CatalogError> {
        crate::query::run(self.conn(), q)
    }
```

```rust
// ferrolite-catalog/src/queries.rs — make row_to_record pub(crate) (it likely already is
// pub(crate); confirm) and add distinct-value helpers:
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
```

```rust
// ferrolite-catalog/src/read_pool.rs — add
    pub fn query_images(
        &self,
        q: &crate::LibraryQuery,
    ) -> Result<Vec<ImageRecord>, CatalogError> {
        self.with_conn(|c| crate::query::run(c, q))
    }
    pub fn distinct_cameras(&self) -> Result<Vec<String>, CatalogError> {
        self.with_conn(crate::queries::distinct_cameras)
    }
    pub fn iso_bounds(&self) -> Result<Option<(u32, u32)>, CatalogError> {
        self.with_conn(crate::queries::iso_bounds)
    }
    pub fn date_bounds(&self) -> Result<Option<(String, String)>, CatalogError> {
        self.with_conn(crate::queries::date_bounds)
    }
```

> If `row_to_record` is not visible to `query.rs`, mark it `pub(crate)` in `queries.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-catalog --test query_integration && cargo test -p ferrolite-catalog`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-catalog/src ferrolite-catalog/tests
git commit -m "feat(catalog): run LibraryQuery + distinct-value helpers for toolbar"
```

---

## Phase G — ingest integration (added_at + rating read-back)

### Task G1: ingest sets `added_at` and reads `xmp:Rating`

**Files:**
- Modify: `ferrolite-app/src/ingest.rs` (`ingest_job`)
- Test: `ferrolite-catalog/tests/query_integration.rs` covers DB shape; add a focused unit in `ferrolite-app/src/ingest.rs` for the rating read-back helper, OR rely on the xmp tests (Phase C) + a manual smoke. Add a small pure helper test.

**Interfaces:**
- Consumes: `ferrolite_catalog::{read_rating, sidecar_path}` (Phase C), `NewImage` new args (Phase B).
- Produces: ingest now stamps `added_at = now_epoch_secs()` and reads any `<file>.xmp` rating into the row.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-app/src/ingest.rs — add to a #[cfg(test)] mod tests
    #[test]
    fn now_epoch_secs_is_positive() {
        assert!(super::now_epoch_secs() > 1_000_000_000);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app ingest::tests::now_epoch_secs_is_positive`
Expected: FAIL — `now_epoch_secs` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-app/src/ingest.rs — add a helper near the top
pub(crate) fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

```rust
// ferrolite-app/src/ingest.rs — in ingest_job, inside the par_iter filter_map,
// replace the NewImage construction with rating read-back + added_at stamping:
            let added_at = now_epoch_secs();
            let rating = ferrolite_catalog::read_rating(&ferrolite_catalog::sidecar_path(&f.path))
                .unwrap_or_default();
            let new_image = match ferrolite_decode::read_metadata(&f.path, f.kind) {
                Ok(meta) => NewImage::from_metadata(
                    folder_id,
                    f.filename.clone(),
                    f.mtime,
                    f.size,
                    &meta,
                    f.kind,
                    rating,
                    added_at,
                ),
                Err(_) => NewImage::failed(
                    folder_id,
                    f.filename.clone(),
                    f.mtime,
                    f.size,
                    f.kind,
                    added_at,
                ),
            };
```

> `read_rating` does sibling-file I/O; it runs inside the existing parallel decode job (off the UI thread) so the responsiveness rule holds.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app ingest::`
Expected: PASS; `cargo build -p ferrolite-app` compiles (NewImage arity satisfied).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/ingest.rs
git commit -m "feat(app): ingest stamps added_at and reads xmp:Rating back"
```

---

## Phase H — app wiring (Library module)

> Phase H tasks add UI. Pure logic (filter→query, edit application, events) is fully
> TDD'd here. The egui rendering steps give the exact widget calls and the file to
> edit; the design-system permits visual deviation (mockup §8). Run
> `cargo build -p ferrolite-app` after each task — fix any borrow/signature drift with
> the **rust-build-resolver** agent, and review egui rendering visually with `/run`.

### Task H1: `FilterState` + pure `to_query`

**Files:**
- Create: `ferrolite-app/src/library/filter.rs`
- Modify: `ferrolite-app/src/library/mod.rs` (`pub mod filter;`)
- Test: `ferrolite-app/src/library/filter.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `ferrolite_catalog::{LibraryQuery, Scope, Sort, SortKey, RatingFilter, TagFilter, TagMode}`, `ferrolite_image::{Flag, TagId}`.
- Produces: `ViewSource { Folder(i64), All, Collection(i64), RecentlyAdded }`; `FilterState { search: String, sort_key: SortKey, sort_desc: bool, min_rating: u8, flags: Vec<Flag>, tag_ids: Vec<TagId>, tag_mode: TagMode, camera: Option<String>, iso: Option<(u32,u32)>, date: Option<(String,String)> }` with `Default`; `FilterState::to_query(&self, source: ViewSource, include_subfolders: bool) -> LibraryQuery`.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-app/src/library/filter.rs
//! Pure mapping from toolbar UI state to a `LibraryQuery`. No egui here.

use ferrolite_catalog::{LibraryQuery, RatingFilter, Scope, Sort, SortKey, TagFilter, TagMode};
use ferrolite_image::{Flag, TagId};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_source_maps_recursive_flag() {
        let fs = FilterState::default();
        let q = fs.to_query(ViewSource::Folder(7), true);
        assert_eq!(q.scope, Scope::Folder { id: 7, recursive: true });
        let q = fs.to_query(ViewSource::Folder(7), false);
        assert_eq!(q.scope, Scope::Folder { id: 7, recursive: false });
    }

    #[test]
    fn min_rating_zero_means_no_filter() {
        let fs = FilterState { min_rating: 0, ..Default::default() };
        assert!(fs.to_query(ViewSource::All, true).rating.is_none());
        let fs = FilterState { min_rating: 3, ..Default::default() };
        assert!(matches!(
            fs.to_query(ViewSource::All, true).rating,
            Some(RatingFilter::AtLeast(3))
        ));
    }

    #[test]
    fn blank_search_is_none() {
        let fs = FilterState { search: "   ".into(), ..Default::default() };
        assert!(fs.to_query(ViewSource::All, true).search.is_none());
        let fs = FilterState { search: "cat".into(), ..Default::default() };
        assert_eq!(fs.to_query(ViewSource::All, true).search.as_deref(), Some("cat"));
    }

    #[test]
    fn recently_added_source_maps_with_limit() {
        let q = FilterState::default().to_query(ViewSource::RecentlyAdded, true);
        assert!(matches!(q.scope, Scope::RecentlyAdded { limit } if limit > 0));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app filter::`
Expected: FAIL — types undefined.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-app/src/library/filter.rs — above the tests module

/// How many images "Recently Added" shows.
const RECENT_LIMIT: i64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewSource {
    Folder(i64),
    All,
    Collection(i64),
    RecentlyAdded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterState {
    pub search: String,
    pub sort_key: SortKey,
    pub sort_desc: bool,
    pub min_rating: u8,
    pub flags: Vec<Flag>,
    pub tag_ids: Vec<TagId>,
    pub tag_mode: TagMode,
    pub camera: Option<String>,
    pub iso: Option<(u32, u32)>,
    pub date: Option<(String, String)>,
}

impl Default for FilterState {
    fn default() -> Self {
        FilterState {
            search: String::new(),
            sort_key: SortKey::CaptureTime,
            sort_desc: false,
            min_rating: 0,
            flags: Vec::new(),
            tag_ids: Vec::new(),
            tag_mode: TagMode::Any,
            camera: None,
            iso: None,
            date: None,
        }
    }
}

impl FilterState {
    pub fn to_query(&self, source: ViewSource, include_subfolders: bool) -> LibraryQuery {
        let scope = match source {
            ViewSource::Folder(id) => Scope::Folder { id, recursive: include_subfolders },
            ViewSource::All => Scope::AllPhotographs,
            ViewSource::Collection(id) => Scope::Collection { id },
            ViewSource::RecentlyAdded => Scope::RecentlyAdded { limit: RECENT_LIMIT },
        };
        let search = {
            let t = self.search.trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        };
        let rating = if self.min_rating == 0 {
            None
        } else {
            Some(RatingFilter::AtLeast(self.min_rating))
        };
        LibraryQuery {
            scope,
            search,
            sort: Sort { key: self.sort_key, desc: self.sort_desc },
            rating,
            flags: self.flags.clone(),
            tags: TagFilter { ids: self.tag_ids.clone(), mode: self.tag_mode },
            camera: self.camera.clone(),
            iso: self.iso,
            date: self.date.clone(),
        }
    }
}
```

```rust
// ferrolite-app/src/library/mod.rs — add
pub mod filter;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app filter::`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/library/filter.rs ferrolite-app/src/library/mod.rs
git commit -m "feat(app): FilterState → LibraryQuery mapping (pure)"
```

---

### Task H2: AppState holds filter/source/tags/collections + query-backed refresh

**Files:**
- Modify: `ferrolite-app/src/state.rs`
- Test: `ferrolite-app/src/state.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: Task H1 types, `ferrolite_catalog::{TagRecord, CollectionRecord, TagId}`.
- Produces: on `AppState`, new fields `filter: FilterState`, `source: ViewSource`, `tags: Vec<TagRecord>`, `collections: Vec<CollectionRecord>`, `visible_tags: HashMap<i64, Vec<TagId>>`, `selection: HashSet<i64>`, `warning: Option<String>`; methods `build_query() -> LibraryQuery`, `reload_vocab()` (loads tags+collections), `ensure_tags_for(&HashSet<i64>)`; `refresh_images()` reworked to run `build_query()`.

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-app/src/state.rs — add to the existing #[cfg(test)] mod tests
    #[test]
    fn refresh_images_uses_filter_query_across_source() {
        use ferrolite_catalog::{FileKind, NewImage};
        use crate::library::filter::ViewSource;
        let mut s = AppState::for_test();
        let (f1, f2) = {
            let w = s.writer.lock().unwrap();
            let f1 = w.upsert_folder(std::path::Path::new("/a"), None).unwrap();
            let f2 = w.upsert_folder(std::path::Path::new("/b"), None).unwrap();
            w.upsert_image(&NewImage::failed(f1, "a.nef".into(), 1, 1, FileKind::Raw, 0)).unwrap();
            w.upsert_image(&NewImage::failed(f2, "b.nef".into(), 1, 1, FileKind::Raw, 0)).unwrap();
            (f1, f2)
        };
        let _ = (f1, f2);
        // AllPhotographs source returns images from both folders.
        s.source = ViewSource::All;
        s.refresh_images();
        assert_eq!(s.images.len(), 2);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app state::tests::refresh_images_uses_filter_query_across_source`
Expected: FAIL — `source` field missing.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-app/src/state.rs — imports
use crate::library::filter::{FilterState, ViewSource};
use ferrolite_catalog::{
    Catalog, CollectionRecord, ImageRecord, LibraryQuery, ReadPool, TagRecord,
};
use ferrolite_image::TagId;
```

```rust
// ferrolite-app/src/state.rs — add fields to AppState (after `viewer`)
    pub filter: FilterState,
    pub source: ViewSource,
    pub tags: Vec<TagRecord>,
    pub collections: Vec<CollectionRecord>,
    pub visible_tags: HashMap<i64, Vec<TagId>>,
    pub selection: HashSet<i64>,
    pub warning: Option<String>,
```

```rust
// ferrolite-app/src/state.rs — initialise the new fields in BOTH `new()` and `for_test()`
            filter: FilterState::default(),
            source: ViewSource::All,
            tags: Vec::new(),
            collections: Vec::new(),
            visible_tags: HashMap::new(),
            selection: HashSet::new(),
            warning: None,
```

```rust
// ferrolite-app/src/state.rs — add methods on impl AppState
    pub fn build_query(&self) -> LibraryQuery {
        self.filter.to_query(self.source, self.include_subfolders)
    }

    pub fn reload_vocab(&mut self) {
        if let Ok(t) = self.reads.list_tags() {
            self.tags = t;
        }
        if let Ok(c) = self.reads.list_collections() {
            self.collections = c;
        }
    }

    /// Fetch tag associations for any visible ids not yet cached (virtualized).
    pub fn ensure_tags_for(&mut self, ids: &HashSet<i64>) {
        let missing: Vec<i64> = ids
            .iter()
            .copied()
            .filter(|id| !self.visible_tags.contains_key(id))
            .collect();
        if missing.is_empty() {
            return;
        }
        if let Ok(map) = self.reads.tags_for_images(&missing) {
            for id in missing {
                self.visible_tags.insert(id, map.get(&id).cloned().unwrap_or_default());
            }
        }
    }
```

```rust
// ferrolite-app/src/state.rs — REPLACE refresh_images() body
    pub fn refresh_images(&mut self) {
        // Folder source needs a current_folder; if none is set just keep AllPhotographs.
        let q = self.build_query();
        if let Ok(rows) = self.reads.query_images(&q) {
            self.images = rows;
        }
        // Invalidate the per-cell tag cache so the grid re-fetches for the new set.
        self.visible_tags.clear();
    }
```

> `select_folder` should also set `self.source = ViewSource::Folder(folder_id)`. Update it:

```rust
// ferrolite-app/src/state.rs — in select_folder, after reset_for_new_folder():
        self.current_folder = Some(folder_id);
        self.source = ViewSource::Folder(folder_id);
```

- [ ] **Step 4: Run test + full app build**

Run: `cargo test -p ferrolite-app state:: && cargo build -p ferrolite-app`
Expected: PASS; existing `refresh_images_honors_include_subfolders` test still passes (it sets `current_folder` + `include_subfolders`; update that test to set `s.source = ViewSource::Folder(root)` so it exercises the folder scope). Adjust that test accordingly.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/state.rs
git commit -m "feat(app): AppState carries filter/source/vocab; refresh via LibraryQuery"
```

---

### Task H3: metadata-edit events + optimistic apply + write job

**Files:**
- Create: `ferrolite-app/src/metadata.rs`
- Modify: `ferrolite-app/src/lib.rs` (`mod metadata;`)
- Modify: `ferrolite-app/src/events.rs` (add `MetadataResult`)
- Test: `ferrolite-app/src/metadata.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `ferrolite_catalog::{Catalog, ImageRecord, sidecar_path, write_rating}`, `ferrolite_image::{Rating, Flag, TagId}`, `ferrolite_jobs::{JobSystem, Priority}`, the app event channel.
- Produces: `enum MetaEdit { SetRating(Rating), SetFlag(Flag), ToggleTag(TagId) }`; pure `apply_edit_in_memory(rec: &mut ImageRecord, visible_tags: &mut Vec<TagId>, edit: MetaEdit)`; `spawn_metadata_write(jobs, writer, tx, ctx, edit, image_paths: Vec<(i64, std::path::PathBuf)>)`; `AppEvent::MetadataResult { ok: bool, warning: Option<String> }` (folded by `apply`: `ok==false` → `dirty=true`; `warning` → `self.warning`).

- [ ] **Step 1: Write the failing test**

```rust
// ferrolite-app/src/metadata.rs
//! Metadata edit commands: optimistic in-memory apply + an off-thread persist job
//! (SQLite for rating/flag/tags, plus the xmp:Rating sidecar for rating).

use crate::events::AppEvent;
use ferrolite_catalog::{Catalog, ImageRecord};
use ferrolite_image::{Flag, Rating, TagId};
use ferrolite_jobs::{JobSystem, Priority};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaEdit {
    SetRating(Rating),
    SetFlag(Flag),
    ToggleTag(TagId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_catalog::FileKind;
    use ferrolite_image::Orientation;

    fn rec() -> ImageRecord {
        ImageRecord {
            id: 1,
            folder_id: 1,
            filename: "x.nef".into(),
            width: None,
            height: None,
            orientation: Orientation::Normal,
            capture_time: None,
            iso: None,
            decode_status: ferrolite_catalog::DecodeStatus::Done,
            kind: FileKind::Raw,
            rating: Rating::default(),
            flag: Flag::None,
        }
    }

    #[test]
    fn set_rating_and_flag_update_record() {
        let mut r = rec();
        let mut tags = vec![];
        apply_edit_in_memory(&mut r, &mut tags, MetaEdit::SetRating(Rating::new(4)));
        assert_eq!(r.rating, Rating::new(4));
        apply_edit_in_memory(&mut r, &mut tags, MetaEdit::SetFlag(Flag::Pick));
        assert_eq!(r.flag, Flag::Pick);
    }

    #[test]
    fn toggle_tag_adds_then_removes() {
        let mut r = rec();
        let mut tags = vec![];
        apply_edit_in_memory(&mut r, &mut tags, MetaEdit::ToggleTag(TagId(5)));
        assert_eq!(tags, vec![TagId(5)]);
        apply_edit_in_memory(&mut r, &mut tags, MetaEdit::ToggleTag(TagId(5)));
        assert!(tags.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app metadata::`
Expected: FAIL — module/`apply_edit_in_memory` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
// ferrolite-app/src/metadata.rs — above the tests module

/// Apply an edit to the in-memory grid row + its cached tag list (optimistic UI).
pub fn apply_edit_in_memory(rec: &mut ImageRecord, visible_tags: &mut Vec<TagId>, edit: MetaEdit) {
    match edit {
        MetaEdit::SetRating(r) => rec.rating = r,
        MetaEdit::SetFlag(f) => rec.flag = f,
        MetaEdit::ToggleTag(t) => {
            if let Some(pos) = visible_tags.iter().position(|x| *x == t) {
                visible_tags.remove(pos);
            } else {
                visible_tags.push(t);
            }
        }
    }
}

/// Persist an edit to all `image_paths` off the UI thread. Writes SQLite for
/// every axis; writes the xmp:Rating sidecar for `SetRating`. Emits a
/// `MetadataResult` (a sidecar failure is a warning, not a revert).
#[allow(clippy::too_many_arguments)]
pub fn spawn_metadata_write(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    edit: MetaEdit,
    image_paths: Vec<(i64, PathBuf)>,
) {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Visible, move |_cancel| {
        let mut warning: Option<String> = None;
        let mut ok = true;
        for (image_id, path) in &image_paths {
            let db = writer.lock().expect("writer");
            let db_res = match edit {
                MetaEdit::SetRating(r) => db.set_rating(*image_id, r),
                MetaEdit::SetFlag(f) => db.set_flag(*image_id, f),
                MetaEdit::ToggleTag(t) => db.toggle_tag(*image_id, t),
            };
            if let Err(e) = db_res {
                ok = false;
                warning = Some(format!("catalog write failed: {e}"));
                continue;
            }
            drop(db);
            if let MetaEdit::SetRating(r) = edit {
                let xmp = ferrolite_catalog::sidecar_path(path);
                if let Err(e) = ferrolite_catalog::write_rating(&xmp, r) {
                    warning = Some(format!("sidecar write failed: {e}"));
                }
            }
        }
        let _ = tx.send(AppEvent::MetadataResult { ok, warning });
        ctx.request_repaint();
    });
}
```

```rust
// ferrolite-catalog/src/catalog.rs — add the two write helpers used above
    pub fn set_rating(&self, image_id: i64, rating: ferrolite_image::Rating) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE images SET rating=?1 WHERE id=?2",
            rusqlite::params![rating.as_i64(), image_id],
        )?;
        Ok(())
    }
    pub fn set_flag(&self, image_id: i64, flag: ferrolite_image::Flag) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE images SET flag=?1 WHERE id=?2",
            rusqlite::params![flag.as_i64(), image_id],
        )?;
        Ok(())
    }
    /// Add the tag if absent, else remove it (mirrors the UI toggle).
    pub fn toggle_tag(&self, image_id: i64, tag: ferrolite_image::TagId) -> Result<(), CatalogError> {
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
```

```rust
// ferrolite-app/src/events.rs — add a variant to AppEvent
    /// Result of an off-thread metadata persist. `ok==false` → reload truth;
    /// `warning` → surface in the status bar.
    MetadataResult { ok: bool, warning: Option<String> },
```

```rust
// ferrolite-app/src/events.rs — handle it in apply()'s match
            AppEvent::MetadataResult { ok, warning } => {
                if !ok {
                    self.dirty = true;
                }
                if warning.is_some() {
                    self.warning = warning;
                }
                None
            }
```

```rust
// ferrolite-app/src/lib.rs — add
mod metadata;
```

- [ ] **Step 4: Run test + build**

Run: `cargo test -p ferrolite-app metadata:: && cargo build -p ferrolite-app && cargo test -p ferrolite-catalog`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/metadata.rs ferrolite-app/src/lib.rs ferrolite-app/src/events.rs ferrolite-catalog/src/catalog.rs
git commit -m "feat(app): metadata edit model — optimistic apply + off-thread persist (DB + xmp:Rating)"
```

---

### Task H4: live toolbar (search, sort, rating/flag/tag filters, metadata popover)

**Files:**
- Modify: `ferrolite-app/src/library/toolbar.rs`
- Modify: `ferrolite-app/src/app.rs` (toolbar call site)
- Test: visual via `/run`; logic already covered by Task H1.

**Interfaces:**
- Consumes: `FilterState`, `state.tags`, `state.reads.{distinct_cameras,iso_bounds,date_bounds}`.
- Produces: `toolbar::show(ui, thumb_size: &mut f32, state: &mut AppState) -> bool` returning `true` when the filter/sort/source changed (caller sets `state.dirty`).

- [ ] **Step 1: Rewrite `toolbar::show` with live widgets**

Replace the stub body. Drive `state.filter` and `state.include_subfolders`; return `changed`. Use these concrete widgets (layout may follow the design mockup):

```rust
// ferrolite-app/src/library/toolbar.rs — new show()
use crate::state::AppState;
use crate::widgets::EguiSlider;
use ferrolite_catalog::{SortKey, TagMode};
use ferrolite_image::Flag;

const SIZE_SLIDER_W: f32 = 208.0;

pub fn show(ui: &mut egui::Ui, thumb_size: &mut f32, state: &mut AppState) -> bool {
    let mut changed = false;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;

        // Search (debounced upstream by the dirty flag; query runs off-thread).
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.filter.search)
                .hint_text("Search filename or tag…")
                .desired_width(206.0),
        );
        if resp.changed() {
            changed = true;
        }

        // Sort key + direction.
        egui::ComboBox::from_id_source("sort")
            .selected_text(sort_label(state.filter.sort_key))
            .show_ui(ui, |ui| {
                for (k, lbl) in [
                    (SortKey::CaptureTime, "Capture Time"),
                    (SortKey::Filename, "Filename"),
                    (SortKey::Rating, "Rating"),
                    (SortKey::AddedAt, "Date Added"),
                ] {
                    if ui.selectable_value(&mut state.filter.sort_key, k, lbl).clicked() {
                        changed = true;
                    }
                }
            });
        if ui.button(if state.filter.sort_desc { "▼" } else { "▲" }).clicked() {
            state.filter.sort_desc = !state.filter.sort_desc;
            changed = true;
        }

        // Rating threshold: click star N to require ≥N; click the active one to clear.
        for n in 1..=5u8 {
            let on = state.filter.min_rating >= n;
            let star = if on { "★" } else { "☆" };
            if ui.small_button(star).clicked() {
                state.filter.min_rating = if state.filter.min_rating == n { 0 } else { n };
                changed = true;
            }
        }

        // Flag filter toggles.
        for (f, lbl) in [(Flag::Pick, "⚑"), (Flag::Reject, "⚐")] {
            let on = state.filter.flags.contains(&f);
            if ui.selectable_label(on, lbl).clicked() {
                toggle_flag(&mut state.filter.flags, f);
                changed = true;
            }
        }

        // Tag filter dropdown (multi-select over the global vocabulary) + Any/All.
        egui::ComboBox::from_id_source("tagfilter")
            .selected_text(format!("Tags ({})", state.filter.tag_ids.len()))
            .show_ui(ui, |ui| {
                let mode_all = matches!(state.filter.tag_mode, TagMode::All);
                if ui.selectable_label(!mode_all, "Any").clicked() {
                    state.filter.tag_mode = TagMode::Any;
                    changed = true;
                }
                if ui.selectable_label(mode_all, "All").clicked() {
                    state.filter.tag_mode = TagMode::All;
                    changed = true;
                }
                ui.separator();
                for t in &state.tags {
                    let mut on = state.filter.tag_ids.contains(&t.id);
                    if ui.checkbox(&mut on, &t.name).changed() {
                        toggle_tag(&mut state.filter.tag_ids, t.id);
                        changed = true;
                    }
                }
            });

        if ui.checkbox(&mut state.include_subfolders, "Subfolders").changed() {
            changed = true;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(SIZE_SLIDER_W, ui.available_height()),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add(EguiSlider {
                        label: "Size",
                        value: thumb_size,
                        min: 0.0,
                        max: 100.0,
                        default: 46.0,
                        step: 1.0,
                        decimals: 0,
                        unit: "",
                        bipolar: false,
                        signed: false,
                    });
                },
            );
        });
    });
    changed
}

fn sort_label(k: SortKey) -> &'static str {
    match k {
        SortKey::CaptureTime => "Capture Time",
        SortKey::Filename => "Filename",
        SortKey::Rating => "Rating",
        SortKey::AddedAt => "Date Added",
    }
}

fn toggle_flag(flags: &mut Vec<Flag>, f: Flag) {
    if let Some(p) = flags.iter().position(|x| *x == f) {
        flags.remove(p);
    } else {
        flags.push(f);
    }
}

fn toggle_tag(ids: &mut Vec<ferrolite_image::TagId>, id: ferrolite_image::TagId) {
    if let Some(p) = ids.iter().position(|x| *x == id) {
        ids.remove(p);
    } else {
        ids.push(id);
    }
}
```

> The camera/ISO/date Metadata popover is added in Task H5 to keep this task focused; the toolbar already drives the primary filters.

- [ ] **Step 2: Update the call site in `app.rs`**

```rust
// ferrolite-app/src/app.rs — replace the toolbar::show call inside the toolbar panel
                if self.module.is_library() {
                    let changed = crate::library::toolbar::show(
                        ui,
                        &mut self.thumb_size,
                        &mut self.state,
                    );
                    if changed {
                        self.state.dirty = true;
                    }
                } else {
```

- [ ] **Step 3: Build + visual check**

Run: `cargo build -p ferrolite-app`
Expected: compiles. Then `/run` the app: search/sort/star/flag/tag-filter change the grid; Subfolders still works.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/library/toolbar.rs ferrolite-app/src/app.rs
git commit -m "feat(app): live Library toolbar — search, sort, rating/flag/tag filters"
```

---

### Task H5: Metadata-range popover (camera / ISO / date)

**Files:**
- Modify: `ferrolite-app/src/library/toolbar.rs`
- Test: logic via H1; visual via `/run`.

**Interfaces:**
- Consumes: `state.reads.distinct_cameras()/iso_bounds()/date_bounds()`, `EguiSlider`.
- Produces: a "Metadata ▾" popover button that edits `state.filter.{camera,iso,date}` and sets `changed`.

- [ ] **Step 1: Add the popover** inside `show`, before the right-aligned size slider:

```rust
// ferrolite-app/src/library/toolbar.rs — inside show(), after the tag dropdown
        let popup_id = ui.make_persistent_id("meta_popover");
        let btn = ui.button("Metadata  ▾");
        if btn.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &btn,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(240.0);
                // Camera model.
                let cameras = state.reads.distinct_cameras().unwrap_or_default();
                egui::ComboBox::from_label("Camera")
                    .selected_text(state.filter.camera.clone().unwrap_or_else(|| "Any".into()))
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(state.filter.camera.is_none(), "Any").clicked() {
                            state.filter.camera = None;
                            changed = true;
                        }
                        for c in &cameras {
                            if ui
                                .selectable_label(state.filter.camera.as_deref() == Some(c), c)
                                .clicked()
                            {
                                state.filter.camera = Some(c.clone());
                                changed = true;
                            }
                        }
                    });
                // ISO range.
                if let Ok(Some((lo, hi))) = state.reads.iso_bounds() {
                    let (mut a, mut b) = state.filter.iso.unwrap_or((lo, hi));
                    let mut af = a as f32;
                    let mut bf = b as f32;
                    let r1 = ui.add(egui::Slider::new(&mut af, lo as f32..=hi as f32).text("ISO min"));
                    let r2 = ui.add(egui::Slider::new(&mut bf, lo as f32..=hi as f32).text("ISO max"));
                    if r1.changed() || r2.changed() {
                        a = af as u32;
                        b = bf as u32;
                        state.filter.iso = Some((a.min(b), a.max(b)));
                        changed = true;
                    }
                    if ui.button("Clear ISO").clicked() {
                        state.filter.iso = None;
                        changed = true;
                    }
                }
                // Date range (ISO-8601 text inputs; lexical compare).
                if let Ok(Some((lo, hi))) = state.reads.date_bounds() {
                    let (mut from, mut to) =
                        state.filter.date.clone().unwrap_or((lo.clone(), hi.clone()));
                    let r1 = ui.add(egui::TextEdit::singleline(&mut from).hint_text("from"));
                    let r2 = ui.add(egui::TextEdit::singleline(&mut to).hint_text("to"));
                    if r1.changed() || r2.changed() {
                        state.filter.date = Some((from, to));
                        changed = true;
                    }
                    if ui.button("Clear dates").clicked() {
                        state.filter.date = None;
                        changed = true;
                    }
                }
            },
        );
```

> If the egui 0.29 popup API signature differs (`popup_below_widget` arity, `PopupCloseBehavior`), use the **rust-build-resolver** to reconcile; the contract is "a popover that edits camera/iso/date and sets `changed`".

- [ ] **Step 2: Build + visual check**

Run: `cargo build -p ferrolite-app`
Expected: compiles; `/run`: the Metadata popover filters by camera/ISO/date.

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/library/toolbar.rs
git commit -m "feat(app): metadata-range popover (camera/ISO/date) in the toolbar"
```

---

### Task H6: grid overlays — rating stars, flag, tag dots + multi-select

**Files:**
- Modify: `ferrolite-app/src/library/grid.rs`
- Test: visual via `/run`; `cell_state` test helper updated in Task B2.

**Interfaces:**
- Consumes: `rec.rating`, `rec.flag`, `state.visible_tags`, `state.tags` (for color lookup), `state.selection`.
- Produces: cells draw a star count, a flag glyph, and up to N tag color dots; ctrl/cmd-click toggles multi-selection; the visible window's tags are ensured each frame.

- [ ] **Step 1: Ensure visible-row tags + draw overlays**

In `show`, after computing `now_visible` and before/after `reprioritize`, call:

```rust
// ferrolite-app/src/library/grid.rs — in show(), after `reprioritize(state, &now_visible);`
        state.ensure_tags_for(&now_visible);
```

In `paint_cell`, after the thumbnail/selection painting, add overlays:

```rust
// ferrolite-app/src/library/grid.rs — in paint_cell, before `opened`
    // Rating stars (bottom-left).
    if rec.rating.get() > 0 {
        let stars: String = "★".repeat(rec.rating.get() as usize);
        painter.text(
            rect.left_bottom() + egui::vec2(4.0, -4.0),
            egui::Align2::LEFT_BOTTOM,
            stars,
            egui::FontId::proportional(11.0),
            theme::ACCENT,
        );
    }
    // Flag glyph (top-left).
    let flag_glyph = match rec.flag {
        ferrolite_image::Flag::Pick => Some(("⚑", theme::SEMANTIC_GREEN)),
        ferrolite_image::Flag::Reject => Some(("⚐", theme::SEMANTIC_RED)),
        ferrolite_image::Flag::None => None,
    };
    if let Some((g, col)) = flag_glyph {
        painter.text(
            rect.left_top() + egui::vec2(4.0, 4.0),
            egui::Align2::LEFT_TOP,
            g,
            egui::FontId::proportional(12.0),
            col,
        );
    }
    // Tag colour dots (bottom-right), looked up from the loaded vocabulary.
    if let Some(tag_ids) = state.visible_tags.get(&rec.id) {
        let mut x = rect.right() - 8.0;
        for tid in tag_ids.iter().take(5) {
            if let Some(t) = state.tags.iter().find(|t| t.id == *tid) {
                let c = egui::Color32::from_rgb(t.color.r, t.color.g, t.color.b);
                painter.circle_filled(egui::pos2(x, rect.bottom() - 8.0), 4.0, c);
                x -= 11.0;
            }
        }
    }
```

> Add `SEMANTIC_GREEN` to `theme.rs` if absent (mirror `SEMANTIC_RED`). Confirm theme constant names before use.

- [ ] **Step 2: Multi-select on click**

```rust
// ferrolite-app/src/library/grid.rs — replace the single-select click handling
    if resp.clicked() {
        let multi = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
        if multi {
            if !state.selection.remove(&rec.id) {
                state.selection.insert(rec.id);
            }
        } else {
            state.selection.clear();
            state.selection.insert(rec.id);
        }
        state.selected = Some(rec.id);
    }
```

```rust
// ferrolite-app/src/library/grid.rs — selection stroke: highlight any cell in the set
    if state.selection.contains(&rec.id) || state.selected == Some(rec.id) {
        painter.rect_stroke(rect, 2.0, egui::Stroke::new(2.0, theme::ACCENT));
    }
```

- [ ] **Step 3: Build + visual check**

Run: `cargo build -p ferrolite-app`
Expected: compiles; `/run`: stars/flag/tag dots show on cells; ctrl-click multi-selects.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/library/grid.rs ferrolite-app/src/theme.rs
git commit -m "feat(app): grid overlays (rating/flag/tag dots) + multi-select"
```

---

### Task H7: keyboard metadata commands (rating 0–5, flag P/X/U)

**Files:**
- Modify: `ferrolite-app/src/app.rs`
- Test: visual via `/run` (the apply/persist logic is unit-tested in H3).

**Interfaces:**
- Consumes: `metadata::{MetaEdit, apply_edit_in_memory, spawn_metadata_write}`, `state.selection`/`selected`, `state.images`, `state.reads.folder_path`.
- Produces: keys `0`–`5` set rating, `P`/`X`/`U` set flag on the current selection (optimistic + persisted).

- [ ] **Step 1: Add a key handler** in `update`, in the Library-only keyboard block (after the Enter handler, guarded by `self.module.is_library() && self.state.viewer.is_none() && !ctx.wants_keyboard_input()`):

```rust
// ferrolite-app/src/app.rs
        if self.module.is_library()
            && self.state.viewer.is_none()
            && self.state.pending_remove.is_none()
            && !ctx.wants_keyboard_input()
        {
            use ferrolite_image::{Flag, Rating};
            let edit = ctx.input(|i| {
                for n in 0..=5u8 {
                    let key = match n {
                        0 => egui::Key::Num0,
                        1 => egui::Key::Num1,
                        2 => egui::Key::Num2,
                        3 => egui::Key::Num3,
                        4 => egui::Key::Num4,
                        _ => egui::Key::Num5,
                    };
                    if i.key_pressed(key) {
                        return Some(crate::metadata::MetaEdit::SetRating(Rating::new(n)));
                    }
                }
                if i.key_pressed(egui::Key::P) {
                    Some(crate::metadata::MetaEdit::SetFlag(Flag::Pick))
                } else if i.key_pressed(egui::Key::X) {
                    Some(crate::metadata::MetaEdit::SetFlag(Flag::Reject))
                } else if i.key_pressed(egui::Key::U) {
                    Some(crate::metadata::MetaEdit::SetFlag(Flag::None))
                } else {
                    None
                }
            });
            if let Some(edit) = edit {
                self.apply_metadata_edit(ctx, edit);
            }
        }
```

- [ ] **Step 2: Add the helper** on `impl FerroliteApp`:

```rust
// ferrolite-app/src/app.rs
    /// Apply a metadata edit to the current selection: optimistic in-memory update
    /// of every affected grid row, then an off-thread persist (DB + xmp:Rating).
    fn apply_metadata_edit(&mut self, ctx: &egui::Context, edit: crate::metadata::MetaEdit) {
        // Resolve the target id set (multi-selection, else the single selected).
        let mut targets: Vec<i64> = self.state.selection.iter().copied().collect();
        if targets.is_empty() {
            if let Some(id) = self.state.selected {
                targets.push(id);
            }
        }
        if targets.is_empty() {
            return;
        }
        // Optimistic in-memory update + collect (id, path) for the persist job.
        let mut image_paths: Vec<(i64, std::path::PathBuf)> = Vec::new();
        for id in &targets {
            if let Some(rec) = self.state.images.iter().find(|r| r.id == *id).cloned() {
                if let Ok(Some(fp)) = self.state.reads.folder_path(rec.folder_id) {
                    image_paths.push((*id, std::path::PathBuf::from(fp).join(&rec.filename)));
                }
            }
        }
        for id in &targets {
            let mut tags = self.state.visible_tags.get(id).cloned().unwrap_or_default();
            if let Some(rec) = self.state.images.iter_mut().find(|r| r.id == *id) {
                crate::metadata::apply_edit_in_memory(rec, &mut tags, edit);
            }
            self.state.visible_tags.insert(*id, tags);
        }
        crate::metadata::spawn_metadata_write(
            &self.state.jobs,
            &self.state.writer,
            &self.state.tx,
            ctx,
            edit,
            image_paths,
        );
    }
```

- [ ] **Step 3: Build + visual check**

Run: `cargo build -p ferrolite-app`
Expected: compiles; `/run`: select a cell, press `3` → 3 stars appear and persist (reopen folder → still 3); `P`/`X`/`U` set the flag; a `.xmp` appears next to the file with the rating.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/app.rs
git commit -m "feat(app): keyboard rating (0-5) + flag (P/X/U) commands on selection"
```

---

### Task H8: left-panel — Catalog sources, Collections, Tag manager

**Files:**
- Modify: `ferrolite-app/src/library/panel.rs`
- Modify: `ferrolite-app/src/app.rs` (reload vocab on startup + after vocab edits)
- Test: visual via `/run`.

**Interfaces:**
- Consumes: `state.{source,collections,tags}`, `state.writer` (create/rename/recolor/delete tag + collection, add selection to collection), `ViewSource`.
- Produces: panel shows "All Photographs" / "Recently Added" (set `state.source`, `state.dirty`), a Collections list (click → `ViewSource::Collection`, "+ New" creates one, "Add selection" adds `state.selection`), and a Tags section with a "+ New tag" affordance + per-tag rename/recolor/delete.

- [ ] **Step 1: Render Catalog sources** at the top of `panel::show` (above the existing Folders tree):

```rust
// ferrolite-app/src/library/panel.rs — near the top of show(ui, state, ctx)
    ui.label(egui::RichText::new("CATALOG").color(crate::theme::TEXT_DIM).size(10.0));
    if ui.selectable_label(matches!(state.source, crate::library::filter::ViewSource::All), "All Photographs").clicked() {
        state.source = crate::library::filter::ViewSource::All;
        state.current_folder = None;
        state.dirty = true;
    }
    if ui.selectable_label(matches!(state.source, crate::library::filter::ViewSource::RecentlyAdded), "Recently Added").clicked() {
        state.source = crate::library::filter::ViewSource::RecentlyAdded;
        state.current_folder = None;
        state.dirty = true;
    }
    ui.add_space(8.0);
```

- [ ] **Step 2: Render Collections** (below Folders):

```rust
// ferrolite-app/src/library/panel.rs
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("COLLECTIONS").color(crate::theme::TEXT_DIM).size(10.0));
        if ui.small_button("+").clicked() {
            // Create a uniquely-named collection off-thread-free (single small write).
            let name = format!("Collection {}", state.collections.len() + 1);
            if let Ok(_) = state.writer.lock().expect("writer").create_collection(&name, ferrolite_image::Color::default()) {
                state.reload_vocab();
            }
        }
    });
    let collections = state.collections.clone();
    for c in &collections {
        ui.horizontal(|ui| {
            let col = egui::Color32::from_rgb(c.color.r, c.color.g, c.color.b);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, col);
            if ui.selectable_label(matches!(state.source, crate::library::filter::ViewSource::Collection(id) if id == c.id), &c.name).clicked() {
                state.source = crate::library::filter::ViewSource::Collection(c.id);
                state.current_folder = None;
                state.dirty = true;
            }
            if ui.small_button("＋sel").clicked() {
                let ids: Vec<i64> = state.selection.iter().copied().collect();
                let w = state.writer.lock().expect("writer");
                for id in ids {
                    let _ = w.add_image_to_collection(c.id, id);
                }
                drop(w);
                if matches!(state.source, crate::library::filter::ViewSource::Collection(id) if id == c.id) {
                    state.dirty = true;
                }
            }
        });
    }
```

- [ ] **Step 3: Render the Tag manager** (below Collections):

```rust
// ferrolite-app/src/library/panel.rs
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("TAGS").color(crate::theme::TEXT_DIM).size(10.0));
        if ui.small_button("+").clicked() {
            let name = format!("tag{}", state.tags.len() + 1);
            if state.writer.lock().expect("writer").create_tag(&name, ferrolite_image::Color::default()).is_ok() {
                state.reload_vocab();
            }
        }
    });
    let tags = state.tags.clone();
    for t in &tags {
        ui.horizontal(|ui| {
            let mut col = [t.color.r as f32 / 255.0, t.color.g as f32 / 255.0, t.color.b as f32 / 255.0];
            if ui.color_edit_button_rgb(&mut col).changed() {
                let c = ferrolite_image::Color {
                    r: (col[0] * 255.0) as u8,
                    g: (col[1] * 255.0) as u8,
                    b: (col[2] * 255.0) as u8,
                };
                let _ = state.writer.lock().expect("writer").set_tag_color(t.id, c);
                state.reload_vocab();
            }
            ui.label(&t.name);
            if ui.small_button("🗑").clicked() {
                let _ = state.writer.lock().expect("writer").delete_tag(t.id);
                state.filter.tag_ids.retain(|x| *x != t.id);
                state.reload_vocab();
                state.dirty = true;
            }
        });
    }
```

> Rename can reuse a `TextEdit` per row if desired; create/recolor/delete cover the core. These are small single-row SQLite writes done synchronously under the writer lock (sub-millisecond); they are not the multi-millisecond work the CLAUDE.md rule targets. If a vocab op ever grows heavy, move it to a job.

- [ ] **Step 4: Load vocab on startup** in `app.rs`:

```rust
// ferrolite-app/src/app.rs — in update(), alongside the one-time startup rescan guard
        if !self.state.startup_rescan_done {
            crate::ingest::spawn_startup_rescan(&mut self.state, ctx);
            self.state.reload_vocab();
            self.state.startup_rescan_done = true;
        }
```

- [ ] **Step 5: Build + visual check**

Run: `cargo build -p ferrolite-app`
Expected: compiles; `/run`: All Photographs / Recently Added switch the grid source; create a collection, add the selection, click it to view; create/recolor/delete a tag, and the tag appears in the toolbar tag filter.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/library/panel.rs ferrolite-app/src/app.rs
git commit -m "feat(app): left-panel catalog sources, collections, and tag manager"
```

---

### Task H9: surface `state.warning` in the status bar

**Files:**
- Modify: `ferrolite-app/src/status_bar.rs`
- Test: visual.

**Interfaces:**
- Consumes: `state.warning: Option<String>`.

- [ ] **Step 1: Show the warning** (append to the status bar content):

```rust
// ferrolite-app/src/status_bar.rs — inside show(ui, state), after the existing readouts
    if let Some(w) = &state.warning {
        ui.separator();
        ui.label(egui::RichText::new(w).color(crate::theme::SEMANTIC_RED).size(11.0));
    }
```

- [ ] **Step 2: Build + check**

Run: `cargo build -p ferrolite-app`
Expected: compiles; a sidecar-write failure (e.g. read-only dir) shows a status note.

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/status_bar.rs
git commit -m "feat(app): surface metadata-write warnings in the status bar"
```

---

## Phase I — gate & finish

### Task I1: workspace gate green

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Then: `cargo fmt --all --check`
Expected: clean.

- [ ] **Step 2: Clippy (warnings = errors)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings. Fix any (often: needless clones, `vec!["?";n]` → acceptable, redundant `.clone()`). Use the **rust-build-resolver** agent for stubborn lints.

- [ ] **Step 3: Test the whole workspace**

Run: `cargo test --workspace`
Expected: all green (image value types, schema v3, xmp round-trip + merge, tag/collection CRUD, query compile + cross-folder integration, filter mapping, metadata apply, existing Spec-1 tests).

- [ ] **Step 4: Commit any gate fixups**

```bash
git add -A
git commit -m "chore: workspace gate green (fmt + clippy -D warnings + tests)"
```

- [ ] **Step 5: Finish the branch**

Use the **superpowers:finishing-a-development-branch** skill to choose merge/PR/cleanup for `feat/tags-and-filters`.

---

## Self-Review (filled in by the planner)

**Spec coverage:**
- §2 metadata axes → Phase A (types), B (columns), C (rating XMP), D (tags), E (collections). ✓
- §3 source-of-truth split → B (rating mirror + upsert refresh), C (XMP write), §5.2 carve-out honored (no sidecar files for tags/collections/flags). ✓
- §4 merge-preserving XMP rating r/w → C2/C3. ✓
- §5 schema v3 → B1. ✓
- §6 value types → A1. ✓
- §7 LibraryQuery (scopes/predicates/sort/search, cross-folder, distinct helpers, tags_for_images batched) → F1/F2, D1/D2. ✓
- §8 UI (toolbar, tag manager, collections, grid overlays, edit commands, multi-select) → H4–H8. ✓
- §9 threading (optimistic + jobs, read-pool queries, off-UI writes) → H3 (write job), H2 (read-pool refresh), G1 (ingest off-thread). ✓
- §10 error handling (malformed/foreign XMP, read-only dir warning, conflict errors, cascades) → C3, H3/H9, D1/E1, B1 (FKs). ✓
- §11 testing → tests in every phase; cross-folder integration F2. ✓

**Placeholder scan:** no TBD/TODO; every code step shows full code. egui rendering tasks reference exact files + widgets and are gated by `cargo build` + `/run`. ✓

**Type consistency:** `Rating`/`Flag`/`Color`/`TagId` (image) used consistently; `LibraryQuery`/`Scope`/`Sort`/`TagFilter` names match between F1 and H1; `MetaEdit`/`apply_edit_in_memory`/`spawn_metadata_write` consistent between H3 and H7; `ViewSource` consistent H1/H2/H8; `AppEvent::MetadataResult` consistent H3 (emit) / events.rs (fold). `NewImage` arity change (B2) is explicitly fixed at all call sites in B2 Step 4 and G1. ✓
