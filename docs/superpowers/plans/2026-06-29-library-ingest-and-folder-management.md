# Library Ingest & Folder Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the ferrolite Library usable on a real, mixed, nested photo library — more RAW formats, standard raster ingest (JPEG/PNG/TIFF/WebP/BMP/GIF), a recursive folder tree, and folder removal.

**Architecture:** Extend the existing crate seams without adding new crates. `FileKind` is a new shared vocabulary type in `ferrolite-image`, classified once at ingest and **persisted** in `images.kind` (one small additive v2 migration). `ferrolite-decode` gains a standard-raster route alongside the RAW route, dispatched by `FileKind`, reusing `apply_orientation`. `ferrolite-catalog` walks the whole subtree, wires `folders.parent_id` (column already exists), and adds `remove_folder` + a recursive list query. The app left panel becomes an indented folder tree with roll-up counts, an "include subfolders" toolbar toggle, and a remove affordance with a subtree-confirm dialog.

**Tech Stack:** Rust 2021, egui/eframe, rusqlite (WAL), rayon, walkdir, the `image` crate, `kamadak-exif` (new), `fast_image_resize`, `ferrolite-jobs`.

## Global Constraints

- **rustfmt** default profile, max line width **100**; run `cargo fmt` before every commit.
- **clippy** clean: the workspace sets `clippy::all = warn`; the final gate runs `cargo clippy --workspace --all-targets -- -D warnings`.
- **rusqlite stays pinned at `0.32`** (do not bump — `libsqlite3-sys 0.38`'s `cfg_select!` breaks on stable rustc). `image` stays `default-features = false` at the workspace level; format decoders are enabled **only** on `ferrolite-decode`'s dependency line.
- **`kamadak-exif`** (crate `kamadak-exif`, imported as `exif`) is the only new dependency — permissive (MIT/BSD-2), GPL-compatible.
- **Catalog is a rebuildable cache:** `remove_folder` deletes catalog rows only — **never files on disk**.
- **`ferrolite-jobs` stays photo-agnostic:** all recursion/format/catalog logic lives in `scan`/`decode`/`catalog`/`app`. Do not touch `ferrolite-jobs`.
- **TDD:** write the failing test first, watch it fail, implement minimally, watch it pass, commit. Target **80%+ on non-GPU logic** (egui rendering glue is exempt; its logic is extracted into pure, tested functions).
- Conventional-commit messages; no attribution footer (disabled globally).

---

## File Structure

**`ferrolite-image`** (shared vocabulary)
- Create `src/file_kind.rs` — `FileKind { Raw, Standard }` + `as_i64`/`from_i64`.
- Modify `src/lib.rs` — `mod file_kind; pub use file_kind::FileKind;`.

**`ferrolite-decode`** (decode routes)
- Create `src/orient.rs` — `apply_orientation` (moved from `preview.rs`, `pub(crate)`).
- Create `src/standard.rs` — `decode_preview_standard`, `read_metadata_standard`, EXIF helpers.
- Modify `src/preview.rs` — rename to `decode_preview_raw`, use `crate::orient::apply_orientation`.
- Modify `src/lib.rs` — kind-dispatching `decode_preview(path, kind)` / `read_metadata(path, kind)`; rename the rawler metadata fn to `read_metadata_raw`; wire new modules.
- Modify `src/error.rs` — add `DecodeError::Exif(String)`.
- Modify `Cargo.toml` — add `exif`; enable `image` decoders.
- Modify `tests/decode.rs` — pass `FileKind::Raw` to the changed signatures.

**`ferrolite-catalog`** (tree + persistence)
- Modify `src/scan.rs` — extend `RAW_EXTS`; add `STANDARD_EXTS` + `classify`; rename `RawFile` → `ScannedFile` (+ `kind`); add `scan_tree`, `collect_dirs`; keep `scan_raw_files` (depth-1) returning `ScannedFile`.
- Modify `src/schema.rs` — `SCHEMA_VERSION = 2`; v2 migration adds `images.kind`.
- Modify `src/model.rs` — `NewImage.kind`, `ImageRecord.kind`; `from_metadata`/`failed` take `kind`.
- Modify `src/catalog.rs` — `upsert_folder(path, parent_id)`; `upsert_image` writes `kind`; add `remove_folder`.
- Modify `src/queries.rs` — `IMAGE_COLS` + `kind`; `row_to_record` reads `kind`; add `list_images_recursive`; `list_folders` selects `parent_id`.
- Modify `src/read_pool.rs` — add `list_images_recursive` passthrough.
- Modify `src/ingest.rs` — recursive ingest via `scan_tree` + `collect_dirs`; pass `kind`.
- Modify `src/lib.rs` — `FolderRecord.parent_id`; export `ScannedFile`, `scan_tree`, `classify`, `collect_dirs`; re-export `FileKind`.
- Modify `tests/read_pool.rs`, `tests/catalog.rs` — `upsert_folder(path, None)`.

**`ferrolite-app`** (UI + orchestration)
- Create `src/library/folder_tree.rs` — pure tree build + roll-up (`FolderNode`, `build_forest`).
- Modify `src/state.rs` — `include_subfolders`, `expanded_folders`, `pending_remove`; `refresh_images` query selection; `remove_folder_cascade`.
- Modify `src/ingest.rs` — recursive ingest; thread `kind` through decode + `NewImage` + thumbnails.
- Modify `src/library/panel.rs` — render the tree, expand/collapse, remove affordance.
- Modify `src/library/toolbar.rs` — "Include subfolders" toggle.
- Modify `src/library/mod.rs` — `pub mod folder_tree;`.
- Modify `src/app.rs` — pass the toggle to the toolbar (mark dirty on change); render the confirm dialog.
- Modify `src/bin/bench_browse.rs` — thread `FileKind::Raw` through the changed signatures.

---

## Task 1: `FileKind` + format classifier + recursive scan

**Files:**
- Create: `ferrolite-image/src/file_kind.rs`
- Modify: `ferrolite-image/src/lib.rs`
- Modify: `ferrolite-catalog/src/scan.rs`
- Modify: `ferrolite-catalog/src/lib.rs` (exports)
- Test: unit tests inside `ferrolite-image/src/file_kind.rs` and `ferrolite-catalog/src/scan.rs`

**Interfaces:**
- Produces:
  - `ferrolite_image::FileKind` — `pub enum FileKind { Raw, Standard }`; `fn as_i64(self) -> i64` (`Raw=0`, `Standard=1`); `fn from_i64(v: i64) -> FileKind` (`1 => Standard`, else `Raw`). Derives `Debug, Clone, Copy, PartialEq, Eq`.
  - `ferrolite_catalog::classify(path: &Path) -> Option<FileKind>`.
  - `ferrolite_catalog::ScannedFile { pub path: PathBuf, pub filename: String, pub mtime: i64, pub size: i64, pub kind: FileKind }` (replaces `RawFile`).
  - `ferrolite_catalog::scan_tree(root: &Path) -> Vec<ScannedFile>` (full-depth, classified, stat'd).
  - `ferrolite_catalog::scan_raw_files(folder: &Path) -> Vec<ScannedFile>` (depth-1, RAW only — unchanged behavior, now returns `ScannedFile` with `kind = Raw`).
  - `ferrolite_catalog::collect_dirs(files: &[ScannedFile], root: &Path) -> Vec<PathBuf>` — every file's parent + ancestors up to `root`, dedup, ordered parent-first.
- Consumes: `walkdir::WalkDir`, `std::path::{Path, PathBuf}`, `ferrolite_image::FileKind`.

- [ ] **Step 1: Write the failing test for `FileKind`**

Create `ferrolite-image/src/file_kind.rs` with only its test module:

```rust
//! RAW-vs-standard classification of an ingested image. Persisted in the
//! catalog (`images.kind`) so consumers route decode without re-inferring.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_i64() {
        assert_eq!(FileKind::from_i64(FileKind::Raw.as_i64()), FileKind::Raw);
        assert_eq!(
            FileKind::from_i64(FileKind::Standard.as_i64()),
            FileKind::Standard
        );
    }

    #[test]
    fn unknown_i64_defaults_to_raw() {
        assert_eq!(FileKind::from_i64(0), FileKind::Raw);
        assert_eq!(FileKind::from_i64(99), FileKind::Raw);
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p ferrolite-image file_kind`
Expected: FAIL (compile error — `FileKind` not defined).

- [ ] **Step 3: Implement `FileKind`**

Prepend to `ferrolite-image/src/file_kind.rs` (above the test module):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Raw,
    Standard,
}

impl FileKind {
    pub fn as_i64(self) -> i64 {
        match self {
            FileKind::Raw => 0,
            FileKind::Standard => 1,
        }
    }

    pub fn from_i64(v: i64) -> FileKind {
        match v {
            1 => FileKind::Standard,
            _ => FileKind::Raw,
        }
    }
}
```

Add to `ferrolite-image/src/lib.rs` (next to the other `mod`/`pub use` lines):

```rust
mod file_kind;
pub use file_kind::FileKind;
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p ferrolite-image file_kind`
Expected: PASS (2 tests).

- [ ] **Step 5: Write the failing tests for `classify` + `scan_tree`**

Add a test module at the bottom of `ferrolite-catalog/src/scan.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn classify_recognizes_raw_standard_and_skips_others() {
        assert_eq!(classify(Path::new("a.NEF")), Some(FileKind::Raw));
        assert_eq!(classify(Path::new("a.cr3")), Some(FileKind::Raw));
        assert_eq!(classify(Path::new("b.JPG")), Some(FileKind::Standard));
        assert_eq!(classify(Path::new("b.png")), Some(FileKind::Standard));
        assert_eq!(classify(Path::new("c.txt")), None);
        assert_eq!(classify(Path::new("noext")), None);
    }

    #[test]
    fn scan_tree_walks_subfolders_and_tags_kind() {
        let root = std::env::temp_dir().join(format!("ferro-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("top.NEF"), b"x").unwrap();
        fs::write(root.join("sub").join("nested.jpg"), b"x").unwrap();
        fs::write(root.join("sub").join("note.txt"), b"x").unwrap();

        let mut files = scan_tree(&root);
        files.sort_by(|a, b| a.filename.cmp(&b.filename));
        assert_eq!(files.len(), 2, "txt skipped, two images found");
        assert_eq!(files[0].filename, "nested.jpg");
        assert_eq!(files[0].kind, FileKind::Standard);
        assert_eq!(files[1].filename, "top.NEF");
        assert_eq!(files[1].kind, FileKind::Raw);

        let dirs = collect_dirs(&files, &root);
        assert!(dirs.contains(&root));
        assert!(dirs.contains(&root.join("sub")));
        // Parent-first ordering: root precedes its child.
        let root_pos = dirs.iter().position(|d| d == &root).unwrap();
        let sub_pos = dirs.iter().position(|d| d == &root.join("sub")).unwrap();
        assert!(root_pos < sub_pos);

        let _ = fs::remove_dir_all(&root);
    }
}
```

- [ ] **Step 6: Run them to verify they fail**

Run: `cargo test -p ferrolite-catalog scan`
Expected: FAIL (compile error — `classify`, `scan_tree`, `collect_dirs`, `FileKind` not in scope).

- [ ] **Step 7: Implement the scan changes**

Rewrite `ferrolite-catalog/src/scan.rs` above the test module to:

```rust
//! Filesystem scan: enumerate supported image files with their stat info.
//! No DB access — reusable by the synchronous `ingest_folder` and by the app's
//! job-driven ingest.

use ferrolite_image::FileKind;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// RAW extensions we ingest (lowercased). Extend as camera coverage grows.
const RAW_EXTS: &[&str] = &[
    "nef", "nrw", "cr2", "cr3", "crw", "arw", "sr2", "srf", "raf", "rw2", "orf", "pef", "dng",
    "raw", "rwl", "iiq", "3fr", "erf", "mef", "mos", "kdc", "dcr", "srw", "x3f", "gpr", "fff",
    "cap", "rwz", "bay", "cs1", "ari", "dcs",
];

/// Standard raster extensions decoded via the `image` crate (lowercased).
const STANDARD_EXTS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp", "bmp", "gif"];

fn ext_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

pub fn is_raw(path: &Path) -> bool {
    ext_lower(path)
        .map(|e| RAW_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

/// Classify a path as a RAW or standard raster, or `None` if unsupported.
pub fn classify(path: &Path) -> Option<FileKind> {
    let e = ext_lower(path)?;
    if RAW_EXTS.contains(&e.as_str()) {
        Some(FileKind::Raw)
    } else if STANDARD_EXTS.contains(&e.as_str()) {
        Some(FileKind::Standard)
    } else {
        None
    }
}

/// One supported file with the stat fields the catalog keys incremental rescan
/// on, plus its classified kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub filename: String,
    pub mtime: i64,
    pub size: i64,
    pub kind: FileKind,
}

fn stat(path: &Path) -> Option<(i64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    let size = meta.len() as i64;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((mtime, size))
}

fn to_scanned(path: &Path, kind: FileKind) -> Option<ScannedFile> {
    let (mtime, size) = stat(path)?;
    Some(ScannedFile {
        path: path.to_path_buf(),
        filename: path.file_name()?.to_string_lossy().to_string(),
        mtime,
        size,
        kind,
    })
}

/// Enumerate RAW files directly in `folder` (depth 1). Kept for the headless
/// benchmark and back-compat; all results have `kind == Raw`.
pub fn scan_raw_files(folder: &Path) -> Vec<ScannedFile> {
    WalkDir::new(folder)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file() && is_raw(e.path()))
        .filter_map(|e| to_scanned(e.path(), FileKind::Raw))
        .collect()
}

/// Recursively enumerate every supported image in the subtree rooted at `root`,
/// each tagged with its classified `FileKind`.
pub fn scan_tree(root: &Path) -> Vec<ScannedFile> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| classify(e.path()).and_then(|k| to_scanned(e.path(), k)))
        .collect()
}

/// All directories that must become `folders` rows: every file's parent plus
/// its ancestors up to (and including) `root`, deduplicated and ordered
/// parent-first (so a parent row exists before its child upserts).
pub fn collect_dirs(files: &[ScannedFile], root: &Path) -> Vec<PathBuf> {
    let mut set: BTreeSet<PathBuf> = BTreeSet::new();
    set.insert(root.to_path_buf());
    for f in files {
        let mut cur = f.path.parent();
        while let Some(dir) = cur {
            set.insert(dir.to_path_buf());
            if dir == root {
                break;
            }
            cur = dir.parent();
        }
    }
    let mut dirs: Vec<PathBuf> = set.into_iter().collect();
    dirs.sort_by_key(|p| p.components().count());
    dirs
}
```

Update `ferrolite-catalog/src/lib.rs` exports — replace the `scan` re-export line with:

```rust
pub use scan::{classify, collect_dirs, is_raw, scan_raw_files, scan_tree, ScannedFile};
pub use ferrolite_image::FileKind;
```

- [ ] **Step 8: Run them to verify they pass**

Run: `cargo test -p ferrolite-catalog scan`
Expected: PASS (`classify_…` + `scan_tree_…`). (Other catalog code may not yet compile — that's fine if `scan` tests are run via `--lib`; if the crate fails to build because `RawFile` is referenced elsewhere, proceed to Step 9 first, then re-run.)

- [ ] **Step 9: Fix the one in-crate `RawFile` reference**

In `ferrolite-catalog/src/ingest.rs`, the local `to_process` binding names the old type. Change line ~25 `Vec<crate::RawFile>` to `Vec<crate::ScannedFile>`. (The full recursive rewrite of `ingest.rs` happens in Task 3; this keeps the crate compiling now.)

- [ ] **Step 10: Verify the crate builds and commit**

Run: `cargo test -p ferrolite-image -p ferrolite-catalog --lib`
Expected: PASS (existing catalog lib tests + the new scan tests). Then:

```bash
cargo fmt
git add ferrolite-image/src ferrolite-catalog/src
git commit -m "feat(scan): FileKind classifier + recursive scan_tree/collect_dirs"
```

---

## Task 2: Standard-raster decode route in `ferrolite-decode`

**Files:**
- Create: `ferrolite-decode/src/orient.rs`
- Create: `ferrolite-decode/src/standard.rs`
- Modify: `ferrolite-decode/src/preview.rs`, `src/lib.rs`, `src/error.rs`, `Cargo.toml`
- Modify: `ferrolite-decode/tests/decode.rs`
- Test: `ferrolite-decode/tests/standard.rs` (new)

**Interfaces:**
- Consumes: `ferrolite_image::{FileKind, ImageBuffer, Orientation, PixelFormat}`, the `image` crate, `exif` (kamadak-exif).
- Produces:
  - `ferrolite_decode::decode_preview(path: &Path, kind: FileKind) -> Result<ImageBuffer, DecodeError>` (kind-dispatching; **signature changed** — was one arg).
  - `ferrolite_decode::read_metadata(path: &Path, kind: FileKind) -> Result<Metadata, DecodeError>` (kind-dispatching; **signature changed**).
  - `ferrolite_decode::{decode_preview_standard, read_metadata_standard}` (path-only, standard route).
  - `crate::orient::apply_orientation(img: image::DynamicImage, o: Orientation) -> image::DynamicImage` (`pub(crate)`).
  - `DecodeError::Exif(String)`.

- [ ] **Step 1: Add dependencies**

In `Cargo.toml` (workspace root) add to `[workspace.dependencies]`:

```toml
kamadak-exif = "0.6"
```

In `ferrolite-decode/Cargo.toml`, change the `image` line and add `exif`:

```toml
image = { workspace = true, features = ["jpeg", "png", "tiff", "webp", "bmp", "gif"] }
exif = { package = "kamadak-exif", workspace = true }
```

Run: `cargo fetch` — Expected: resolves `kamadak-exif 0.6`.

- [ ] **Step 2: Add the `Exif` error variant**

In `ferrolite-decode/src/error.rs`, add a variant inside `DecodeError`:

```rust
    #[error("exif error: {0}")]
    Exif(String),
```

- [ ] **Step 3: Extract `apply_orientation` into `orient.rs`**

Create `ferrolite-decode/src/orient.rs`:

```rust
//! EXIF-orientation application shared by the RAW and standard decode routes.

use ferrolite_image::Orientation;
use image::DynamicImage;

/// Apply an EXIF orientation to a decoded image using the `image` crate's
/// rotate/flip ops. (rotate90/270 are clockwise in the `image` crate.)
pub(crate) fn apply_orientation(img: DynamicImage, o: Orientation) -> DynamicImage {
    match o {
        Orientation::Normal => img,
        Orientation::FlipH => img.fliph(),
        Orientation::Rotate180 => img.rotate180(),
        Orientation::FlipV => img.flipv(),
        Orientation::Transpose => img.rotate90().fliph(),
        Orientation::Rotate90 => img.rotate90(),
        Orientation::Transverse => img.rotate270().fliph(),
        Orientation::Rotate270 => img.rotate270(),
    }
}
```

In `ferrolite-decode/src/preview.rs`: delete the local `apply_orientation` fn, rename `pub fn decode_preview` to `pub fn decode_preview_raw`, and replace the call with `crate::orient::apply_orientation(dynimg, …)`. The top of the file becomes:

```rust
use crate::error::{rawler as rawler_err, DecodeError};
use crate::orient::apply_orientation;
use ferrolite_image::{ImageBuffer, Orientation, PixelFormat};
use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;
use std::path::Path;

/// Decode an upright RGB8 preview from a RAW's embedded JPEG (see module note).
pub fn decode_preview_raw(path: &Path) -> Result<ImageBuffer, DecodeError> {
```

(Keep the body identical; it already calls `apply_orientation(dynimg, …)`, now resolved from `crate::orient`. Remove the old `use image::DynamicImage;` and the trailing `fn apply_orientation` block.)

- [ ] **Step 4: Write the failing standard-decode tests**

Create `ferrolite-decode/tests/standard.rs`:

```rust
use ferrolite_image::FileKind;
use std::path::PathBuf;

/// Write a tiny PNG to a temp path and return it.
fn temp_png() -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("ferro-dec-{}-{}.png", std::process::id(), "a"));
    let img = image::RgbImage::from_pixel(8, 4, image::Rgb([10, 20, 30]));
    img.save(&path).expect("write png");
    path
}

#[test]
fn standard_metadata_reports_dimensions_and_empty_make() {
    let path = temp_png();
    let meta = ferrolite_decode::read_metadata(&path, FileKind::Standard).expect("meta");
    assert_eq!(meta.width, 8);
    assert_eq!(meta.height, 4);
    assert_eq!(meta.make, "", "PNG has no EXIF make");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn standard_preview_is_nonempty_rgb8() {
    let path = temp_png();
    let buf = ferrolite_decode::decode_preview(&path, FileKind::Standard).expect("preview");
    assert_eq!(buf.width, 8);
    assert_eq!(buf.height, 4);
    assert!(!buf.pixels.is_empty());
    let _ = std::fs::remove_file(&path);
}
```

- [ ] **Step 5: Run them to verify they fail**

Run: `cargo test -p ferrolite-decode --test standard`
Expected: FAIL (compile error — `read_metadata` takes one arg; `standard` route missing).

- [ ] **Step 6: Implement `standard.rs`**

Create `ferrolite-decode/src/standard.rs`:

```rust
//! Standard-raster decode route (JPEG/PNG/TIFF/WebP/BMP/GIF) via the `image`
//! crate, with EXIF read through `kamadak-exif`. Mirrors the RAW route's
//! products so everything downstream stays format-agnostic.

use crate::error::DecodeError;
use crate::metadata::Metadata;
use crate::orient::apply_orientation;
use ferrolite_image::{ImageBuffer, Orientation, PixelFormat};
use std::path::Path;

fn read_exif(path: &Path) -> Option<exif::Exif> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = std::io::BufReader::new(file);
    exif::Reader::new().read_from_container(&mut buf).ok()
}

fn ascii(e: &exif::Exif, tag: exif::Tag) -> Option<String> {
    e.get_field(tag, exif::In::PRIMARY)
        .map(|f| f.display_value().to_string())
}

fn uint(e: &exif::Exif, tag: exif::Tag) -> Option<u32> {
    e.get_field(tag, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
}

fn rational_f32(e: &exif::Exif, tag: exif::Tag) -> Option<f32> {
    e.get_field(tag, exif::In::PRIMARY)
        .and_then(|f| match &f.value {
            exif::Value::Rational(v) => v.first().map(|r| r.to_f32()),
            _ => None,
        })
}

fn orientation_of(e: &exif::Exif) -> Orientation {
    uint(e, exif::Tag::Orientation)
        .map(|v| Orientation::from_exif(v as u16))
        .unwrap_or(Orientation::Normal)
}

/// Read dimensions (cheap header read) + any present EXIF for a standard raster.
pub fn read_metadata_standard(path: &Path) -> Result<Metadata, DecodeError> {
    let (width, height) = image::image_dimensions(path)?;
    let exif = read_exif(path);
    let (make, model, orientation, iso, aperture, shutter, focal_length, capture_time, lens) =
        match exif.as_ref() {
            Some(e) => (
                ascii(e, exif::Tag::Make).unwrap_or_default(),
                ascii(e, exif::Tag::Model).unwrap_or_default(),
                orientation_of(e),
                uint(e, exif::Tag::PhotographicSensitivity),
                rational_f32(e, exif::Tag::FNumber),
                rational_f32(e, exif::Tag::ExposureTime),
                rational_f32(e, exif::Tag::FocalLength),
                ascii(e, exif::Tag::DateTimeOriginal),
                ascii(e, exif::Tag::LensModel),
            ),
            None => (
                String::new(),
                String::new(),
                Orientation::Normal,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
        };
    Ok(Metadata {
        make,
        model,
        width,
        height,
        orientation,
        iso,
        aperture,
        shutter,
        focal_length,
        capture_time,
        lens,
    })
}

/// Decode an upright RGB8 preview from a standard raster (orientation applied).
pub fn decode_preview_standard(path: &Path) -> Result<ImageBuffer, DecodeError> {
    let dynimg = image::open(path)?;
    let orientation = read_exif(path)
        .as_ref()
        .map(orientation_of)
        .unwrap_or(Orientation::Normal);
    let oriented = apply_orientation(dynimg, orientation);
    let rgb = oriented.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    Ok(ImageBuffer::new(w, h, PixelFormat::Rgb8, rgb.into_raw())
        .expect("RGB8 buffer length is w*h*3 by construction"))
}
```

- [ ] **Step 7: Wire the kind-dispatching entry points**

Rewrite the top of `ferrolite-decode/src/lib.rs` module declarations + re-exports and the `read_metadata` fn. Replace lines 4–17 (the `mod`/`pub use`/imports) and the existing `pub fn read_metadata` with:

```rust
mod error;
mod metadata;
mod orient;
mod preview;
mod raw;
mod standard;

pub use error::DecodeError;
pub use metadata::Metadata;
pub use raw::{decode_full, RawDecoded};
pub use standard::{decode_preview_standard, read_metadata_standard};

use ferrolite_image::{FileKind, ImageBuffer, Orientation};
use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;
use std::path::Path;

use crate::error::rawler as rawler_err;

/// Decode an upright RGB8 preview, routed by `kind`.
pub fn decode_preview(path: &Path, kind: FileKind) -> Result<ImageBuffer, DecodeError> {
    match kind {
        FileKind::Raw => preview::decode_preview_raw(path),
        FileKind::Standard => standard::decode_preview_standard(path),
    }
}

/// Read camera/exposure metadata + dimensions, routed by `kind`.
pub fn read_metadata(path: &Path, kind: FileKind) -> Result<Metadata, DecodeError> {
    match kind {
        FileKind::Raw => read_metadata_raw(path),
        FileKind::Standard => standard::read_metadata_standard(path),
    }
}

/// rawler `Rational` → f32.
fn rat(n: u32, d: u32) -> Option<f32> {
    if d == 0 {
        None
    } else {
        Some(n as f32 / d as f32)
    }
}

/// RAW metadata via rawler (dimensions from a `dummy` decode; no pixel work).
fn read_metadata_raw(path: &Path) -> Result<Metadata, DecodeError> {
```

Keep the **body** of the old `read_metadata` (the rawler logic from the original `lib.rs`) exactly as-is under the new `read_metadata_raw` name — it still uses `rat`, `rawler_err`, `Orientation`, `RawSource`, `RawDecodeParams`, `Path`. (The old `pub use preview::decode_preview;` line is gone; `decode_preview` is now the dispatcher above.)

- [ ] **Step 8: Update the existing RAW decode tests for the new signatures**

In `ferrolite-decode/tests/decode.rs`, add `use ferrolite_image::FileKind;` at the top and pass `FileKind::Raw`:
- `read_metadata(&fixture())` → `read_metadata(&fixture(), FileKind::Raw)` (both call sites: lines ~16 and ~39).
- `decode_preview(&fixture())` → `decode_preview(&fixture(), FileKind::Raw)` (line ~28).

- [ ] **Step 9: Run the full decode test suite**

Run: `cargo test -p ferrolite-decode`
Expected: PASS — existing RAW tests (`decode.rs`) + new standard tests (`standard.rs`).

- [ ] **Step 10: Commit**

```bash
cargo fmt
git add Cargo.toml ferrolite-decode
git commit -m "feat(decode): standard-raster route (image + kamadak-exif), kind dispatch"
```

---

## Task 3: Catalog tree — `kind` migration, parent wiring, recursive query, remove

**Files:**
- Modify: `ferrolite-catalog/src/schema.rs`, `src/model.rs`, `src/catalog.rs`, `src/queries.rs`, `src/read_pool.rs`, `src/ingest.rs`, `src/lib.rs`
- Modify: `ferrolite-catalog/tests/catalog.rs`, `tests/read_pool.rs`
- Test: `ferrolite-catalog/tests/tree.rs` (new — recursive ingest, recursive list, remove)

**Interfaces:**
- Consumes: `ferrolite_image::FileKind`, `ferrolite_decode::{read_metadata, decode_preview}` (kind-aware), `scan_tree`, `collect_dirs`.
- Produces:
  - `NewImage { …, pub kind: FileKind }`; `NewImage::from_metadata(folder_id, filename, mtime, size, meta, kind)`; `NewImage::failed(folder_id, filename, mtime, size, kind)`.
  - `ImageRecord { …, pub kind: FileKind }`.
  - `Catalog::upsert_folder(&self, path: &Path, parent_id: Option<i64>) -> Result<i64, CatalogError>`.
  - `Catalog::remove_folder(&self, folder_id: i64) -> Result<(), CatalogError>`.
  - `Catalog::list_images_recursive(&self, folder_id: i64) -> Result<Vec<ImageRecord>, CatalogError>`; same on `ReadPool`.
  - `FolderRecord { id, path, pub parent_id: Option<i64>, image_count }`.

- [ ] **Step 1: Write the failing migration + model test**

Add to `ferrolite-catalog/tests/catalog.rs` (it already opens a `Catalog`; mirror its helpers). Append:

```rust
#[test]
fn kind_round_trips_and_schema_is_v2() {
    use ferrolite_catalog::FileKind;
    let cat = ferrolite_catalog::Catalog::open_in_memory().unwrap();
    assert_eq!(cat.schema_version().unwrap(), 2);
    let folder = cat
        .upsert_folder(std::path::Path::new("/photos/a"), None)
        .unwrap();
    let raw = ferrolite_catalog::NewImage::failed(folder, "r.nef".into(), 1, 1, FileKind::Raw);
    let std_ = ferrolite_catalog::NewImage::failed(folder, "s.jpg".into(), 1, 1, FileKind::Standard);
    cat.upsert_image(&raw).unwrap();
    cat.upsert_image(&std_).unwrap();
    let mut rows = cat.list_images(folder).unwrap();
    rows.sort_by(|a, b| a.filename.cmp(&b.filename));
    assert_eq!(rows[0].kind, FileKind::Raw); // r.nef
    assert_eq!(rows[1].kind, FileKind::Standard); // s.jpg
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p ferrolite-catalog --test catalog kind_round_trips`
Expected: FAIL (compile errors — `upsert_folder` arity, `NewImage::failed` arity, `ImageRecord.kind`, schema v1).

- [ ] **Step 3: Bump the schema to v2**

In `ferrolite-catalog/src/schema.rs`: set `pub const SCHEMA_VERSION: i64 = 2;` and add a v2 block after the v1 block (before the `debug_assert_eq!`):

```rust
    if version < 2 {
        conn.execute_batch("ALTER TABLE images ADD COLUMN kind INTEGER NOT NULL DEFAULT 0;")?;
        version = 2;
    }
```

(Existing rows default to `0 = Raw`, correct: pre-v2 ingest was RAW-only.)

- [ ] **Step 4: Add `kind` to the model**

In `ferrolite-catalog/src/model.rs`:
- Add `use ferrolite_image::{FileKind, Orientation};` (replace the existing `use ferrolite_image::Orientation;`).
- Add `pub kind: FileKind,` to both `NewImage` and `ImageRecord`.
- Change `from_metadata` to take a trailing `kind: FileKind` param and set `kind` in the returned struct.
- Change `failed` to take a trailing `kind: FileKind` param and set `kind` (instead of an implied RAW).

The two constructors become:

```rust
    pub fn from_metadata(
        folder_id: i64,
        filename: String,
        mtime: i64,
        size: i64,
        meta: &ferrolite_decode::Metadata,
        kind: FileKind,
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
        }
    }

    pub fn failed(folder_id: i64, filename: String, mtime: i64, size: i64, kind: FileKind) -> Self {
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
        }
    }
```

- [ ] **Step 5: Persist + read `kind`; add `parent_id` to `upsert_folder`; add `remove_folder`**

In `ferrolite-catalog/src/catalog.rs`:

Replace `upsert_folder`:

```rust
    /// Insert a folder by path with an optional parent, or return the existing
    /// id. A non-null `parent_id` overwrites; a `None` keeps any existing parent
    /// (so re-opening a subfolder as a root does not orphan its wired parent).
    pub fn upsert_folder(
        &self,
        path: &Path,
        parent_id: Option<i64>,
    ) -> Result<i64, CatalogError> {
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
```

In `upsert_image`, add `kind` to the column list, the `VALUES`, and the `DO UPDATE SET`, and bind `img.kind.as_i64()`:

```rust
        self.conn().execute(
            "INSERT INTO images
               (folder_id, filename, mtime, size, camera_make, camera_model,
                width, height, orientation, capture_time, iso, decode_status, kind)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(folder_id, filename) DO UPDATE SET
                mtime=?3, size=?4, camera_make=?5, camera_model=?6, width=?7,
                height=?8, orientation=?9, capture_time=?10, iso=?11,
                decode_status=?12, kind=?13",
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
            ],
        )?;
```

Add `list_images_recursive` + `remove_folder` methods on `Catalog`:

```rust
    pub fn list_images_recursive(
        &self,
        folder_id: i64,
    ) -> Result<Vec<ImageRecord>, CatalogError> {
        crate::queries::list_images_recursive(self.conn(), folder_id)
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
```

- [ ] **Step 6: Read `kind`, add the recursive query, expose `parent_id`**

In `ferrolite-catalog/src/queries.rs`:
- Add `use ferrolite_image::FileKind;` near the top imports.
- Append `kind` to `IMAGE_COLS`:

```rust
const IMAGE_COLS: &str = "id, folder_id, filename, width, height, orientation,
                          capture_time, iso, decode_status, kind";
```

- In `row_to_record`, read the new column (index 9) and set `kind`:

```rust
    let status: i64 = row.get(8)?;
    let kind: i64 = row.get(9)?;
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
    })
```

- Add the recursive list query:

```rust
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
```

- Add `parent_id` to `list_folders`:

```rust
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
```

In `ferrolite-catalog/src/read_pool.rs`, add the passthrough next to `list_images`:

```rust
    pub fn list_images_recursive(&self, folder_id: i64) -> Result<Vec<ImageRecord>, CatalogError> {
        self.with_conn(|c| crate::queries::list_images_recursive(c, folder_id))
    }
```

In `ferrolite-catalog/src/lib.rs`, add `parent_id` to `FolderRecord`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRecord {
    pub id: i64,
    pub path: String,
    pub parent_id: Option<i64>,
    pub image_count: u64,
}
```

- [ ] **Step 7: Make the synchronous `ingest_folder` recursive + kind-aware**

Rewrite `ferrolite-catalog/src/ingest.rs`'s `ingest_folder` and `decode_one` to walk the tree, build the folder map, and thread `kind`:

```rust
use crate::catalog::Catalog;
use crate::error::CatalogError;
use crate::model::{IngestSummary, NewImage};
use crate::thumbnail::{generate_thumbnail, Thumbnail, ThumbnailStore};
use crate::ScannedFile;
use ferrolite_image::FileKind;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct Decoded {
    folder_id: i64,
    filename: String,
    mtime: i64,
    size: i64,
    kind: FileKind,
    outcome: Result<(NewImage, Thumbnail), String>,
}

impl Catalog {
    /// Ingest a folder and **all its subfolders** (Model B). Every directory in
    /// the subtree becomes a `folders` row with `parent_id` wired; each image is
    /// keyed to its actual directory.
    pub fn ingest_folder(&self, path: &Path) -> Result<IngestSummary, CatalogError> {
        let mut summary = IngestSummary::default();
        let files = crate::scan_tree(path);

        // 1) Create folder rows top-down, wiring parent_id.
        let mut dir_ids: HashMap<PathBuf, i64> = HashMap::new();
        for dir in crate::collect_dirs(&files, path) {
            let parent_id = dir.parent().and_then(|p| dir_ids.get(p).copied());
            let id = self.upsert_folder(&dir, parent_id)?;
            dir_ids.insert(dir, id);
        }

        // 2) Decide which files need (re)ingest.
        let mut to_process: Vec<(&ScannedFile, i64)> = Vec::new();
        for f in &files {
            summary.scanned += 1;
            let folder_id = match f.path.parent().and_then(|p| dir_ids.get(p)) {
                Some(id) => *id,
                None => continue,
            };
            if self.needs_reingest(folder_id, &f.filename, f.mtime, f.size)? {
                to_process.push((f, folder_id));
            } else {
                summary.skipped += 1;
            }
        }

        // 3) Decode + thumbnail in parallel (no DB access).
        let decoded: Vec<Decoded> = to_process
            .into_par_iter()
            .map(|(f, folder_id)| Decoded {
                folder_id,
                filename: f.filename.clone(),
                mtime: f.mtime,
                size: f.size,
                kind: f.kind,
                outcome: decode_one(&f.path, folder_id, &f.filename, f.mtime, f.size, f.kind),
            })
            .collect();

        // 4) Write rows + thumbnails serially.
        for d in decoded {
            match d.outcome {
                Ok((new_image, thumb)) => {
                    let id = self.upsert_image(&new_image)?;
                    self.put_thumbnail(id, &thumb)?;
                    summary.added += 1;
                }
                Err(msg) => {
                    eprintln!("ferrolite-catalog: decode failed for {}: {msg}", d.filename);
                    let failed =
                        NewImage::failed(d.folder_id, d.filename, d.mtime, d.size, d.kind);
                    self.upsert_image(&failed)?;
                    summary.failed += 1;
                }
            }
        }

        for (dir, id) in &dir_ids {
            let _ = dir;
            self.conn().execute(
                "UPDATE folders SET last_scanned = ?1 WHERE id = ?2",
                rusqlite::params![now_secs(), id],
            )?;
        }
        Ok(summary)
    }
}

fn decode_one(
    path: &std::path::Path,
    folder_id: i64,
    filename: &str,
    mtime: i64,
    size: i64,
    kind: FileKind,
) -> Result<(NewImage, Thumbnail), String> {
    let meta = ferrolite_decode::read_metadata(path, kind).map_err(|e| e.to_string())?;
    let preview = ferrolite_decode::decode_preview(path, kind).map_err(|e| e.to_string())?;
    let thumb = generate_thumbnail(&preview).map_err(|e| e.to_string())?;
    let new_image =
        NewImage::from_metadata(folder_id, filename.to_string(), mtime, size, &meta, kind);
    Ok((new_image, thumb))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

- [ ] **Step 8: Update the existing catalog tests for the new `upsert_folder` arity**

In `ferrolite-catalog/tests/catalog.rs` and `tests/read_pool.rs`, every `upsert_folder(path)` becomes `upsert_folder(path, None)`. (Call sites: `catalog.rs` lines ~49, 72, 76, 97, 150, 187; `read_pool.rs` line ~38. Use search/replace `upsert_folder(` → confirm each gets a `, None` argument before the closing paren.)

- [ ] **Step 9: Run the migration/model test**

Run: `cargo test -p ferrolite-catalog --test catalog kind_round_trips`
Expected: PASS.

- [ ] **Step 10: Write the failing tree/recursive/remove integration test**

Create `ferrolite-catalog/tests/tree.rs`:

```rust
//! Recursive ingest tree: parent wiring, per-directory keying, recursive list,
//! and subtree removal. Uses tiny generated PNGs (no rawler needed).

use ferrolite_catalog::{Catalog, ReadPool};
use std::path::PathBuf;

fn make_png(path: &std::path::Path) {
    let img = image::RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3]));
    img.save(path).unwrap();
}

fn nested_fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!("ferro-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("2024")).unwrap();
    std::fs::create_dir_all(root.join("2025")).unwrap();
    // Same filename in two sibling subfolders — must NOT collide.
    make_png(&root.join("2024").join("IMG_001.png"));
    make_png(&root.join("2025").join("IMG_001.png"));
    make_png(&root.join("top.png"));
    root
}

#[test]
fn recursive_ingest_wires_tree_and_keys_per_directory() {
    let root = nested_fixture();
    let db = std::env::temp_dir().join(format!("ferro-tree-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let cat = Catalog::open(&db).unwrap();

    let summary = cat.ingest_folder(&root).unwrap();
    assert_eq!(summary.added, 3, "two nested + one top-level");

    let reads = ReadPool::open(&db, 1).unwrap();
    let folders = reads.list_folders().unwrap();
    // root + 2024 + 2025 = 3 folder rows.
    assert_eq!(folders.len(), 3);
    let root_row = folders.iter().find(|f| f.parent_id.is_none()).unwrap();
    let children: Vec<_> = folders
        .iter()
        .filter(|f| f.parent_id == Some(root_row.id))
        .collect();
    assert_eq!(children.len(), 2, "2024 and 2025 are children of root");

    // Duplicate filename in two folders both ingested.
    let total: u64 = folders.iter().map(|f| f.image_count).sum();
    assert_eq!(total, 3);

    // Recursive list over root returns the whole subtree; direct returns 1.
    let recursive = reads.list_images_recursive(root_row.id).unwrap();
    assert_eq!(recursive.len(), 3);
    let direct = reads.list_images(root_row.id).unwrap();
    assert_eq!(direct.len(), 1, "only top.png is directly in root");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn remove_folder_deletes_subtree_only() {
    let root = nested_fixture();
    let db = std::env::temp_dir().join(format!("ferro-rm-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let cat = Catalog::open(&db).unwrap();
    cat.ingest_folder(&root).unwrap();

    let reads = ReadPool::open(&db, 1).unwrap();
    let folders = reads.list_folders().unwrap();
    let child_2024 = folders
        .iter()
        .find(|f| f.path.ends_with("2024"))
        .unwrap()
        .id;

    cat.remove_folder(child_2024).unwrap();

    let after = reads.list_folders().unwrap();
    assert_eq!(after.len(), 2, "2024 removed; root + 2025 remain");
    assert!(after.iter().all(|f| !f.path.ends_with("2024")));
    // Its image is gone; total drops from 3 to 2.
    let total: u64 = after.iter().map(|f| f.image_count).sum();
    assert_eq!(total, 2);

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&db);
}
```

- [ ] **Step 11: Run the tree tests to verify they fail, then pass**

Run: `cargo test -p ferrolite-catalog --test tree`
Expected first run: PASS once Steps 3–8 compile (these exercise already-implemented code). If `image` is not a dev-dependency of `ferrolite-catalog`, add to `ferrolite-catalog/Cargo.toml` under `[dev-dependencies]`: `image = { workspace = true, features = ["png"] }`. Re-run — Expected: PASS (2 tests).

- [ ] **Step 12: Build the whole catalog crate and commit**

Run: `cargo test -p ferrolite-catalog`
Expected: PASS (all unit + integration tests). Then:

```bash
cargo fmt
git add ferrolite-catalog
git commit -m "feat(catalog): recursive folder tree, kind column (v2), remove_folder"
```

---

## Task 4: App — recursive ingest, folder tree UI, subfolder toggle, remove dialog

**Files:**
- Create: `ferrolite-app/src/library/folder_tree.rs`
- Modify: `ferrolite-app/src/state.rs`, `src/ingest.rs`, `src/library/panel.rs`, `src/library/toolbar.rs`, `src/library/mod.rs`, `src/app.rs`, `src/bin/bench_browse.rs`
- Test: unit tests in `folder_tree.rs` and `state.rs`

**Interfaces:**
- Consumes: `ferrolite_catalog::{FolderRecord, FileKind, scan_tree, collect_dirs}`, `ferrolite_decode` (kind-aware), the existing `JobSystem`.
- Produces:
  - `folder_tree::FolderNode { id: i64, name: String, rollup_count: u64, depth: usize, has_children: bool }` (flattened, render-ready) + `folder_tree::flatten(folders: &[FolderRecord], expanded: &HashSet<i64>) -> Vec<FolderNode>`.
  - `folder_tree::subtree_count(folders: &[FolderRecord], folder_id: i64) -> u64`.
  - `AppState.include_subfolders: bool` (default `true`), `AppState.expanded_folders: HashSet<i64>`, `AppState.pending_remove: Option<PendingRemove>`.
  - `AppState::remove_folder_cascade(&mut self, folder_id: i64)`.

- [ ] **Step 1: Write the failing `folder_tree` tests**

Create `ferrolite-app/src/library/folder_tree.rs` with the test module first:

```rust
//! Pure folder-tree construction for the left panel: turns the catalog's flat
//! `FolderRecord` list into a depth-ordered, roll-up-counted, render-ready list
//! honoring the user's expanded/collapsed set. No egui here (unit-tested).

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_catalog::FolderRecord;
    use std::collections::HashSet;

    fn rec(id: i64, path: &str, parent: Option<i64>, count: u64) -> FolderRecord {
        FolderRecord {
            id,
            path: path.into(),
            parent_id: parent,
            image_count: count,
        }
    }

    fn fixture() -> Vec<FolderRecord> {
        // root(1)[2 direct] -> 2024(2)[3], 2025(3)[5]; 2024 -> jan(4)[7]
        vec![
            rec(1, "/p", None, 2),
            rec(2, "/p/2024", Some(1), 3),
            rec(3, "/p/2025", Some(1), 5),
            rec(4, "/p/2024/jan", Some(2), 7),
        ]
    }

    #[test]
    fn subtree_count_rolls_up_descendants() {
        let f = fixture();
        assert_eq!(subtree_count(&f, 1), 2 + 3 + 5 + 7);
        assert_eq!(subtree_count(&f, 2), 3 + 7);
        assert_eq!(subtree_count(&f, 4), 7);
    }

    #[test]
    fn flatten_collapsed_root_hides_descendants() {
        let f = fixture();
        let expanded = HashSet::new(); // nothing expanded
        let nodes = flatten(&f, &expanded);
        assert_eq!(nodes.len(), 1, "only root shows when collapsed");
        assert_eq!(nodes[0].id, 1);
        assert_eq!(nodes[0].depth, 0);
        assert!(nodes[0].has_children);
        assert_eq!(nodes[0].rollup_count, 17);
    }

    #[test]
    fn flatten_expanded_shows_children_in_order() {
        let f = fixture();
        let expanded: HashSet<i64> = [1, 2].into_iter().collect();
        let nodes = flatten(&f, &expanded);
        let ids: Vec<i64> = nodes.iter().map(|n| n.id).collect();
        // root, 2024 (depth1), jan (depth2), 2025 (depth1) — sorted by path.
        assert_eq!(ids, vec![1, 2, 4, 3]);
        let jan = nodes.iter().find(|n| n.id == 4).unwrap();
        assert_eq!(jan.depth, 2);
        assert!(!jan.has_children);
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p ferrolite-app folder_tree`
Expected: FAIL (compile error — module not declared / fns missing).

- [ ] **Step 3: Implement `folder_tree.rs`**

Prepend above the test module:

```rust
use ferrolite_catalog::FolderRecord;
use std::collections::HashSet;

/// A render-ready tree row: indentation depth, roll-up count, expandability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderNode {
    pub id: i64,
    pub name: String,
    pub rollup_count: u64,
    pub depth: usize,
    pub has_children: bool,
}

fn children_of(folders: &[FolderRecord], parent: Option<i64>) -> Vec<&FolderRecord> {
    let mut kids: Vec<&FolderRecord> = folders
        .iter()
        .filter(|f| f.parent_id == parent)
        .collect();
    kids.sort_by(|a, b| a.path.cmp(&b.path));
    kids
}

/// Sum of `image_count` over `folder_id` and all its descendants.
pub fn subtree_count(folders: &[FolderRecord], folder_id: i64) -> u64 {
    let own = folders
        .iter()
        .find(|f| f.id == folder_id)
        .map(|f| f.image_count)
        .unwrap_or(0);
    let kids: u64 = folders
        .iter()
        .filter(|f| f.parent_id == Some(folder_id))
        .map(|f| subtree_count(folders, f.id))
        .sum();
    own + kids
}

fn leaf_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Flatten the forest into a depth-ordered list, descending only into folders
/// present in `expanded`. Roots = rows whose `parent_id` is `None` or points
/// outside the set.
pub fn flatten(folders: &[FolderRecord], expanded: &HashSet<i64>) -> Vec<FolderNode> {
    let id_set: HashSet<i64> = folders.iter().map(|f| f.id).collect();
    let mut out = Vec::new();
    // Roots: parent_id None or a parent not in this set.
    let roots: Vec<&FolderRecord> = {
        let mut r: Vec<&FolderRecord> = folders
            .iter()
            .filter(|f| f.parent_id.map(|p| !id_set.contains(&p)).unwrap_or(true))
            .collect();
        r.sort_by(|a, b| a.path.cmp(&b.path));
        r
    };
    for root in roots {
        push_node(folders, root, 0, expanded, &mut out);
    }
    out
}

fn push_node(
    folders: &[FolderRecord],
    node: &FolderRecord,
    depth: usize,
    expanded: &HashSet<i64>,
    out: &mut Vec<FolderNode>,
) {
    let kids = children_of(folders, Some(node.id));
    out.push(FolderNode {
        id: node.id,
        name: leaf_name(&node.path),
        rollup_count: subtree_count(folders, node.id),
        depth,
        has_children: !kids.is_empty(),
    });
    if expanded.contains(&node.id) {
        for kid in kids {
            push_node(folders, kid, depth + 1, expanded, out);
        }
    }
}
```

Add `pub mod folder_tree;` to `ferrolite-app/src/library/mod.rs`.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test -p ferrolite-app folder_tree`
Expected: PASS (3 tests).

- [ ] **Step 5: Write the failing state test (subfolder query selection + cascade remove)**

Add to the `tests` module in `ferrolite-app/src/state.rs`:

```rust
    #[test]
    fn refresh_images_honors_include_subfolders() {
        use ferrolite_catalog::{FileKind, NewImage};
        let mut s = AppState::for_test();
        // Build root(parent None) with a child; one image in each.
        let (root, child) = {
            let w = s.writer.lock().unwrap();
            let root = w.upsert_folder(std::path::Path::new("/p"), None).unwrap();
            let child = w
                .upsert_folder(std::path::Path::new("/p/sub"), Some(root))
                .unwrap();
            w.upsert_image(&NewImage::failed(root, "a.nef".into(), 1, 1, FileKind::Raw))
                .unwrap();
            w.upsert_image(&NewImage::failed(child, "b.jpg".into(), 1, 1, FileKind::Standard))
                .unwrap();
            (root, child)
        };
        let _ = child;
        s.current_folder = Some(root);

        s.include_subfolders = false;
        s.refresh_images();
        assert_eq!(s.images.len(), 1, "direct view: only root's image");

        s.include_subfolders = true;
        s.refresh_images();
        assert_eq!(s.images.len(), 2, "recursive view: root + child images");
    }

    #[test]
    fn remove_folder_cascade_clears_current_when_inside_subtree() {
        use ferrolite_catalog::{FileKind, NewImage};
        let mut s = AppState::for_test();
        let (root, child) = {
            let w = s.writer.lock().unwrap();
            let root = w.upsert_folder(std::path::Path::new("/p"), None).unwrap();
            let child = w
                .upsert_folder(std::path::Path::new("/p/sub"), Some(root))
                .unwrap();
            w.upsert_image(&NewImage::failed(child, "b.jpg".into(), 1, 1, FileKind::Standard))
                .unwrap();
            (root, child)
        };
        s.current_folder = Some(child);
        s.remove_folder_cascade(root); // removing an ancestor of current
        assert_eq!(s.current_folder, None, "current cleared when in removed subtree");
        assert!(s.reads.list_folders().unwrap().is_empty());
    }
```

- [ ] **Step 6: Run them to verify they fail**

Run: `cargo test -p ferrolite-app --lib state`
Expected: FAIL (compile error — `include_subfolders`, `remove_folder_cascade` missing).

- [ ] **Step 7: Extend `AppState`**

In `ferrolite-app/src/state.rs`:
- Add fields to the struct:

```rust
    /// Recursive (subtree) vs direct folder view. Default true (on).
    pub include_subfolders: bool,
    /// Folder ids whose children are shown in the left-panel tree.
    pub expanded_folders: HashSet<i64>,
    /// A folder pending a remove-confirmation (set when it has subfolders).
    pub pending_remove: Option<PendingRemove>,
```

- Add the struct near the top of the file:

```rust
/// A folder awaiting remove confirmation (shown in a modal).
#[derive(Debug, Clone)]
pub struct PendingRemove {
    pub id: i64,
    pub name: String,
    pub subtree_count: u64,
}
```

- Initialize the three fields in **both** `new()` and `for_test()`:

```rust
            include_subfolders: true,
            expanded_folders: HashSet::new(),
            pending_remove: None,
```

- Change `refresh_images` to pick the query by the flag:

```rust
    pub fn refresh_images(&mut self) {
        if let Some(folder_id) = self.current_folder {
            let rows = if self.include_subfolders {
                self.reads.list_images_recursive(folder_id)
            } else {
                self.reads.list_images(folder_id)
            };
            if let Ok(rows) = rows {
                self.images = rows;
            }
        }
    }
```

- Add the cascade-remove helper (UI thread; quick write):

```rust
    /// Remove a folder subtree from the catalog (cache only). If the current
    /// folder is inside the removed subtree, reset selection/jobs first.
    pub fn remove_folder_cascade(&mut self, folder_id: i64) {
        let removed_set = self.subtree_ids(folder_id);
        if self
            .current_folder
            .map(|c| removed_set.contains(&c))
            .unwrap_or(false)
        {
            self.reset_for_new_folder();
            self.current_folder = None;
        }
        if let Err(e) = self.writer.lock().expect("writer").remove_folder(folder_id) {
            eprintln!("ferrolite: remove_folder failed: {e}");
            return;
        }
        self.expanded_folders.remove(&folder_id);
        self.dirty = true;
    }

    /// Folder ids in the subtree rooted at `folder_id`, computed from the flat
    /// folder list (read pool).
    fn subtree_ids(&self, folder_id: i64) -> HashSet<i64> {
        let folders = self.reads.list_folders().unwrap_or_default();
        let mut out = HashSet::new();
        let mut stack = vec![folder_id];
        while let Some(id) = stack.pop() {
            if out.insert(id) {
                for f in &folders {
                    if f.parent_id == Some(id) {
                        stack.push(f.id);
                    }
                }
            }
        }
        out
    }
```

- [ ] **Step 8: Run the state tests to verify they pass**

Run: `cargo test -p ferrolite-app --lib state`
Expected: PASS (existing reset/select tests + the two new ones).

- [ ] **Step 9: Make the app ingest recursive + kind-aware**

Rewrite `ferrolite-app/src/ingest.rs`. Key changes: `spawn_ingest` upserts the root with `None`; `ingest_job` walks `scan_tree`, builds the folder map, keys each file to its directory, and threads `kind` into `read_metadata`, `NewImage`, and the thumbnail job.

Replace the imports + `spawn_ingest` root upsert + `ingest_job` body. The new file:

```rust
//! Job orchestration: recursive folder ingest (Interactive) fans out per-image
//! thumbnail jobs (Background). All photo/catalog knowledge lives here; the
//! `ferrolite-jobs` crate stays domain-agnostic.

use crate::events::AppEvent;
use crate::state::AppState;
use ferrolite_catalog::{
    collect_dirs, scan_tree, Catalog, DecodeStatus, FileKind, NewImage, ReadPool, Thumbnail,
};
use ferrolite_jobs::{CancelToken, JobSystem, Priority};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

pub fn spawn_ingest(state: &mut AppState, ctx: &egui::Context, folder: PathBuf) {
    state.reset_for_new_folder();

    let writer = Arc::clone(&state.writer);
    let reads = Arc::clone(&state.reads);
    let jobs = Arc::clone(&state.jobs);
    let tx = state.tx.clone();
    let ctx = ctx.clone();

    let folder_id = match writer.lock().expect("writer").upsert_folder(&folder, None) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("ferrolite: upsert_folder failed: {e}");
            return;
        }
    };
    state.current_folder = Some(folder_id);

    let jobs_for_closure = Arc::clone(&jobs);
    let handle = jobs.submit(Priority::Interactive, move |cancel| {
        ingest_job(folder, writer, reads, jobs_for_closure, tx, ctx, cancel);
    });
    state.ingest_handle = Some(handle);
}

#[allow(clippy::too_many_arguments)]
fn ingest_job(
    folder: PathBuf,
    writer: Arc<Mutex<Catalog>>,
    reads: Arc<ReadPool>,
    jobs: Arc<JobSystem>,
    tx: Sender<AppEvent>,
    ctx: egui::Context,
    cancel: &CancelToken,
) {
    let files = scan_tree(&folder);

    // Create folder rows top-down, wiring parent_id.
    let mut dir_ids: HashMap<PathBuf, i64> = HashMap::new();
    for dir in collect_dirs(&files, &folder) {
        if cancel.is_cancelled() {
            return;
        }
        let parent_id = dir.parent().and_then(|p| dir_ids.get(p).copied());
        match writer.lock().expect("writer").upsert_folder(&dir, parent_id) {
            Ok(id) => {
                dir_ids.insert(dir, id);
            }
            Err(e) => eprintln!("ferrolite: upsert_folder failed: {e}"),
        }
    }

    // Parallel metadata decode for files needing (re)ingest. No DB writes here.
    let rows: Vec<(NewImage, PathBuf, FileKind)> = files
        .par_iter()
        .filter(|_| !cancel.is_cancelled())
        .filter_map(|f| {
            let folder_id = *f.path.parent().and_then(|p| dir_ids.get(p))?;
            match reads.needs_reingest(folder_id, &f.filename, f.mtime, f.size) {
                Ok(true) => {}
                _ => return None,
            }
            let new_image = match ferrolite_decode::read_metadata(&f.path, f.kind) {
                Ok(meta) => NewImage::from_metadata(
                    folder_id,
                    f.filename.clone(),
                    f.mtime,
                    f.size,
                    &meta,
                    f.kind,
                ),
                Err(_) => NewImage::failed(folder_id, f.filename.clone(), f.mtime, f.size, f.kind),
            };
            Some((new_image, f.path.clone(), f.kind))
        })
        .collect();

    for (new_image, path, kind) in rows {
        if cancel.is_cancelled() {
            break;
        }
        let id = match writer.lock().expect("writer").upsert_image(&new_image) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("ferrolite: upsert_image failed: {e}");
                continue;
            }
        };
        let _ = tx.send(AppEvent::Indexed { added: 1 });
        if new_image.decode_status != DecodeStatus::Failed {
            let job_id = spawn_thumbnail(&jobs, &writer, &tx, &ctx, id, path, kind);
            let _ = tx.send(AppEvent::ThumbRegistered {
                image_id: id,
                job_id,
            });
        }
        ctx.request_repaint();
    }
    let _ = tx.send(AppEvent::IngestDone);
    ctx.request_repaint();
}

/// Headless thumbnail helper: decode preview → resize/encode → persist BLOB.
pub fn thumbnail_blocking(
    writer: &Arc<Mutex<Catalog>>,
    image_id: i64,
    path: &Path,
    kind: FileKind,
) -> Result<Thumbnail, String> {
    let preview = ferrolite_decode::decode_preview(path, kind).map_err(|e| e.to_string())?;
    let thumb = ferrolite_catalog::generate_thumbnail(&preview).map_err(|e| e.to_string())?;
    {
        use ferrolite_catalog::ThumbnailStore;
        writer
            .lock()
            .expect("writer")
            .put_thumbnail(image_id, &thumb)
            .map_err(|e| e.to_string())?;
    }
    Ok(thumb)
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_thumbnail(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    path: PathBuf,
    kind: FileKind,
) -> ferrolite_jobs::JobId {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Background, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        match thumbnail_blocking(&writer, image_id, &path, kind) {
            Ok(thumb) => {
                let _ = tx.send(AppEvent::ThumbReady {
                    image_id,
                    jpeg: thumb.bytes,
                });
            }
            Err(msg) => {
                eprintln!("ferrolite: thumbnail failed for #{image_id}: {msg}");
                let _ = writer
                    .lock()
                    .expect("writer")
                    .set_decode_status(image_id, DecodeStatus::Failed);
                let _ = tx.send(AppEvent::ThumbFailed { image_id });
            }
        }
        ctx.request_repaint();
    })
    .id()
}
```

- [ ] **Step 10: Update `bench_browse.rs` for the new signatures**

In `ferrolite-app/src/bin/bench_browse.rs`:
- Add `FileKind` to the catalog import: `use ferrolite_catalog::{scan_raw_files, Catalog, DecodeStatus, FileKind, NewImage, ThumbnailStore};`.
- `upsert_folder(&folder)` → `upsert_folder(&folder, None)`.
- `read_metadata(&f.path)` → `read_metadata(&f.path, FileKind::Raw)`.
- `NewImage::from_metadata(folder_id, f.filename.clone(), f.mtime, f.size, &meta)` → add `, FileKind::Raw`.
- `NewImage::failed(folder_id, f.filename.clone(), f.mtime, f.size)` → add `, FileKind::Raw`.
- `thumbnail_blocking(&writer, *image_id, path)` (both call sites) → add `, FileKind::Raw`.

- [ ] **Step 11: Render the folder tree in the left panel**

Rewrite `ferrolite-app/src/library/panel.rs`:

```rust
//! Library left panel: Catalog header, Open-folder action, and the folder tree
//! (indented, expandable, roll-up counts) read from the catalog. A ✕ on hover
//! and a right-click "Remove" trigger folder removal (subtree-confirm via state).

use crate::ingest::spawn_ingest;
use crate::library::folder_tree::{flatten, subtree_count};
use crate::state::{AppState, PendingRemove};
use crate::theme;

pub fn show(ui: &mut egui::Ui, state: &mut AppState, ctx: &egui::Context) {
    ui.add_space(8.0);
    ui.colored_label(theme::TEXT_FAINT, "CATALOG");
    ui.label("All Photographs");
    ui.add_space(8.0);

    if ui.button("Open folder…").clicked() {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            spawn_ingest(state, ctx, folder);
        }
    }

    ui.add_space(12.0);
    ui.colored_label(theme::TEXT_FAINT, "FOLDERS");

    let folders = state.reads.list_folders().unwrap_or_default();
    let nodes = flatten(&folders, &state.expanded_folders);

    for node in nodes {
        ui.horizontal(|ui| {
            ui.add_space(node.depth as f32 * 14.0);

            // Expand/collapse triangle (only when the node has children).
            if node.has_children {
                let open = state.expanded_folders.contains(&node.id);
                let arrow = if open { "▾" } else { "▸" };
                if ui.add(egui::Button::new(arrow).frame(false)).clicked() {
                    if open {
                        state.expanded_folders.remove(&node.id);
                    } else {
                        state.expanded_folders.insert(node.id);
                    }
                }
            } else {
                ui.add_space(14.0);
            }

            let selected = state.current_folder == Some(node.id);
            let label = format!("{}  ({})", node.name, node.rollup_count);
            let resp = ui.selectable_label(selected, label);
            if resp.clicked() {
                state.select_folder(node.id);
            }
            // Right-click context menu → Remove.
            resp.context_menu(|ui| {
                if ui.button("Remove from catalog").clicked() {
                    request_remove(state, &folders, node.id, &node.name);
                    ui.close_menu();
                }
            });
            // Hover ✕.
            if resp.hovered() && ui.small_button("✕").clicked() {
                request_remove(state, &folders, node.id, &node.name);
            }
        });
    }
}

/// A leaf folder removes immediately; one with subfolders stages a confirm.
fn request_remove(
    state: &mut AppState,
    folders: &[ferrolite_catalog::FolderRecord],
    id: i64,
    name: &str,
) {
    let has_children = folders.iter().any(|f| f.parent_id == Some(id));
    if has_children {
        state.pending_remove = Some(PendingRemove {
            id,
            name: name.to_string(),
            subtree_count: subtree_count(folders, id),
        });
    } else {
        state.remove_folder_cascade(id);
    }
}
```

- [ ] **Step 12: Add the include-subfolders toggle to the toolbar**

In `ferrolite-app/src/library/toolbar.rs`, change the signature to accept the flag and report a change. Update `show`:

```rust
/// Returns true if the include-subfolders toggle changed this frame.
pub fn show(ui: &mut egui::Ui, thumb_size: &mut f32, include_subfolders: &mut bool) -> bool {
    let mut changed = false;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;

        let mut query = String::new();
        ui.add_enabled(
            false,
            egui::TextEdit::singleline(&mut query)
                .hint_text("Search catalog…")
                .desired_width(206.0),
        );

        ui.label(dim("Sort"));
        ui.add_enabled(false, egui::Button::new("Capture Time  ▾"));

        ui.label(dim("Filter"));
        ui.add_enabled(
            false,
            egui::Button::new(egui::RichText::new("★★★★★").color(crate::theme::TEXT_FAINT)),
        );
        ui.add_enabled(false, egui::Button::new("Metadata  ▾"));

        // Real toggle: include images from subfolders in the grid.
        changed = ui
            .checkbox(include_subfolders, "Subfolders")
            .changed();

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
```

- [ ] **Step 13: Wire the toolbar toggle + confirm dialog in `app.rs`**

In `ferrolite-app/src/app.rs`:
- Update the toolbar call to pass the flag and mark dirty on change:

```rust
                if self.module.is_library() {
                    let changed = crate::library::toolbar::show(
                        ui,
                        &mut self.thumb_size,
                        &mut self.state.include_subfolders,
                    );
                    if changed {
                        self.state.dirty = true;
                    }
                }
```

- After the `CentralPanel` block (before the window-border painter), render the confirm dialog:

```rust
        // Remove-folder confirmation (subtrees only; leaves remove immediately).
        if let Some(pending) = self.state.pending_remove.clone() {
            let mut open = true;
            egui::Window::new("Remove folder from catalog")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Remove “{}” and its subfolders ({} images) from the catalog?",
                        pending.name, pending.subtree_count
                    ));
                    ui.label(
                        egui::RichText::new("Files on disk are not deleted.")
                            .color(theme::TEXT_DIM)
                            .size(11.0),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Remove").clicked() {
                            self.state.remove_folder_cascade(pending.id);
                            self.state.pending_remove = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.pending_remove = None;
                        }
                    });
                });
            if !open {
                self.state.pending_remove = None;
            }
        }
```

- [ ] **Step 14: Build the app and run the full workspace tests**

Run: `cargo build -p ferrolite-app` then `cargo test -p ferrolite-app`
Expected: PASS (folder_tree + state + events + cell_state tests; app + bench binaries compile).

- [ ] **Step 15: Commit**

```bash
cargo fmt
git add ferrolite-app
git commit -m "feat(app): folder tree panel, subfolder toggle, recursive ingest, remove dialog"
```

---

## Task 5: Integration sweep, manual verification & quality gate

**Files:**
- Test: `ferrolite-app/tests/ingest_tree.rs` (new — end-to-end recursive mixed ingest, headless)
- No source changes expected (fix-ups only if the gate finds issues).

**Interfaces:**
- Consumes: everything above, exercised headlessly via `thumbnail_blocking` + `Catalog` (no egui).

- [ ] **Step 1: Write the end-to-end recursive mixed-ingest test**

Create `ferrolite-app/tests/ingest_tree.rs`:

```rust
//! End-to-end: a nested, mixed-format folder ingests into a wired tree with
//! per-directory keying and a working thumbnail for a standard raster — all
//! headless (no egui), mirroring the interactive path's helpers.

use ferrolite_app::ingest::thumbnail_blocking;
use ferrolite_catalog::{collect_dirs, scan_tree, Catalog, FileKind, NewImage, ReadPool};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn make_png(path: &std::path::Path) {
    image::RgbImage::from_pixel(6, 4, image::Rgb([9, 9, 9]))
        .save(path)
        .unwrap();
}

#[test]
fn nested_mixed_ingest_builds_tree_and_thumbnails_standard() {
    let root = std::env::temp_dir().join(format!("ferro-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("a")).unwrap();
    make_png(&root.join("top.png"));
    make_png(&root.join("a").join("inner.jpeg"));

    let db = std::env::temp_dir().join(format!("ferro-e2e-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let writer: Arc<Mutex<Catalog>> = Arc::new(Mutex::new(Catalog::open(&db).unwrap()));

    let files = scan_tree(&root);
    assert_eq!(files.len(), 2);
    let mut dir_ids: HashMap<PathBuf, i64> = HashMap::new();
    for dir in collect_dirs(&files, &root) {
        let parent = dir.parent().and_then(|p| dir_ids.get(p).copied());
        let id = writer.lock().unwrap().upsert_folder(&dir, parent).unwrap();
        dir_ids.insert(dir, id);
    }
    let mut first_id = None;
    for f in &files {
        let folder_id = dir_ids[f.path.parent().unwrap()];
        let img = NewImage::failed(folder_id, f.filename.clone(), f.mtime, f.size, f.kind);
        let _ = img; // ensure kind is carried even on the failed-path constructor
        let meta = ferrolite_decode::read_metadata(&f.path, f.kind).unwrap();
        let row =
            NewImage::from_metadata(folder_id, f.filename.clone(), f.mtime, f.size, &meta, f.kind);
        let id = writer.lock().unwrap().upsert_image(&row).unwrap();
        first_id.get_or_insert((id, f.path.clone(), f.kind));
    }

    // A standard-raster thumbnail decodes + persists.
    let (id, path, kind) = first_id.unwrap();
    assert_eq!(kind, FileKind::Standard);
    thumbnail_blocking(&writer, id, &path, kind).expect("thumbnail");

    let reads = ReadPool::open(&db, 1).unwrap();
    let root_id = reads
        .list_folders()
        .unwrap()
        .into_iter()
        .find(|f| f.parent_id.is_none())
        .unwrap()
        .id;
    assert_eq!(reads.list_images_recursive(root_id).unwrap().len(), 2);
    assert!(reads.get_thumbnail(id).unwrap().is_some());

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&db);
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p ferrolite-app --test ingest_tree`
Expected: PASS. (If `image` is not a dev-dependency of `ferrolite-app`, add `image = { workspace = true, features = ["png", "jpeg"] }` under `[dev-dependencies]` in `ferrolite-app/Cargo.toml`.)

- [ ] **Step 3: Full workspace test run**

Run: `cargo test --workspace`
Expected: PASS across all crates. Fix any failures before proceeding (do not edit tests to pass — fix the implementation, unless a test encodes a wrong expectation).

- [ ] **Step 4: Clippy + fmt gate**

Run: `cargo fmt --all -- --check` then `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no diffs, no warnings. Fix any clippy findings (common ones here: needless `clone`, `map_or` suggestions). Re-run until clean.

- [ ] **Step 5: Manual verification (real app)**

Run: `cargo run -p ferrolite-app`
Verify by eye:
- Open a **nested, mixed** folder → the left panel shows an indented tree with triangles; counts are roll-ups; expanding/collapsing works.
- Standard rasters (JPEG/PNG) **and** RAWs both show thumbnails in the grid.
- Toggle **Subfolders** off → grid shows only the selected folder's direct images; on → whole subtree.
- ✕ / right-click **Remove** on a leaf removes immediately; on a folder with subfolders a confirm dialog appears naming the subtree count; confirming refreshes the tree; the files remain on disk.

Record the result in the commit message body.

- [ ] **Step 6: Final commit**

```bash
cargo fmt
git add -A
git commit -m "test(app): end-to-end nested mixed ingest; verify library on real tree

Manual verification: nested mixed folder ingests to a wired tree; RAW+standard
thumbnails render; subfolders toggle switches recursive/direct; leaf removes
immediately, subtree removal confirms; disk files untouched."
```

---

## Self-Review (completed during planning)

**Spec coverage:**
- Item 1 (more RAW formats) → Task 1 Step 7 (extended `RAW_EXTS`). ✓
- Item 2 (standard ingest, `FileKind`, non-RAW decode route, reuse `apply_orientation`) → Task 1 (`FileKind`, classify) + Task 2 (`orient`/`standard`, kind dispatch). ✓
- Item 3 (recursive Model-B tree, parent_id, per-dir keying, recursive CTE, roll-up counts) → Task 1 (`scan_tree`/`collect_dirs`) + Task 3 (`upsert_folder(parent_id)`, recursive ingest, `list_images_recursive`, `list_folders.parent_id`) + Task 4 (`folder_tree`, panel). ✓
- Item 4 (`remove_folder`, affordance, ReadPool refresh) → Task 3 (`remove_folder`) + Task 4 (`request_remove`, `remove_folder_cascade`, dialog). ✓
- UX decisions: subfolders on-by-default + toolbar toggle (Task 4 Steps 7/12/13); roll-up-only counts (Task 4 `folder_tree`/panel); confirm-for-subtrees (Task 4 `request_remove` + dialog). ✓
- Persisted `kind` via v2 migration (spec §3) → Task 3 Steps 3–6. ✓
- No-migration-for-tree, cache-only remove, jobs untouched (contracts) → honored throughout. ✓

**Placeholder scan:** No TBD/TODO; every code step shows full code; test code is concrete. ✓

**Type consistency:** `decode_preview(path, kind)`/`read_metadata(path, kind)` consistent across decode, catalog ingest, app ingest, bench. `NewImage::from_metadata(..., kind)` / `failed(..., kind)` consistent across model, catalog ingest, app ingest, bench, tests. `upsert_folder(path, parent_id)` consistent across catalog, app, bench, tests. `FolderRecord.parent_id`, `ImageRecord.kind` consistent across queries, folder_tree, tests. `flatten`/`subtree_count` names match between `folder_tree.rs` and `panel.rs`. ✓
