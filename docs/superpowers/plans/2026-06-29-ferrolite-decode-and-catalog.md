# Ferrolite Speed Core — Plan 2: Decode & Catalog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the three data-layer crates of Spec 1 — `ferrolite-image` (shared pixel/orientation vocabulary), `ferrolite-decode` (wraps `rawler` for preview/full/metadata), and `ferrolite-catalog` (SQLite DAM: schema, ingest, thumbnails, queries) — so the app can **ingest a folder, generate thumbnails, and query them**, proving **G1 (browse speed)** groundwork.

**Architecture:** `ferrolite-image` is a zero-dependency vocabulary crate (engine-transferable tier). `ferrolite-decode` turns a RAW path into three *independently consumable* products (contract §3): an upright RGB8 preview `ImageBuffer`, a full `RawDecoded`, and `Metadata`. `ferrolite-catalog` owns a `rusqlite` connection behind a repository API, runs `user_version` migrations, stores 256px JPEG thumbnails as BLOBs through a swappable `ThumbnailStore` trait, and ingests a folder with a rayon-parallel decode/resize stage feeding serial DB writes. No job scheduler and no UI yet (Plans 3–4); ingest is a synchronous function *structured* so Plan 3 can wrap per-file work in cancellable priority jobs without API churn.

**Tech Stack:** Rust, `rawler` 0.7.2, `rusqlite` 0.40 (`bundled`), `fast_image_resize` 6.0, `image` 0.25 (JPEG), `rayon`, `walkdir`, `thiserror`.

## Global Constraints

- **License:** GPL-3.0-only (`license.workspace = true`). (Architecture map §2.)
- **Crate tiers / dependency purity:** `ferrolite-image` is engine-transferable → it may depend **only** on permissive crates; this plan gives it **zero** dependencies. `ferrolite-decode`/`ferrolite-catalog` are photo-domain → they may pull `rawler` (LGPL-2.1) and other deps; the binary stays GPL-3.0. (Architecture map §3.)
- **Catalog is a cache, never source of truth:** source of truth = files on disk + sidecars. A missing/corrupt DB must be rebuildable by re-ingesting. Schema mismatch → rebuild. (Architecture map §5.2, design §4.)
- **Decode yields separable products:** `{ preview, full, metadata }` are independently callable so the two-tier load path (Plan 4) can show the preview without waiting on full decode. (Architecture map §5.3.)
- **MSRV:** rawler 0.7.2 requires **Rust 1.88**. Bump `workspace.package.rust-version` from `1.85` to `1.88`. `rust-toolchain.toml` is `channel = "stable"` (already ≥1.88), and CI uses `dtolnay/rust-toolchain@stable`, so no toolchain file change is needed.
- **Thumbnail format = JPEG (q85), 256px max edge.** The design (`speed-core-design.md` §4) names WebP, but the current `image` crate's WebP encoder is **lossless-only** (bloated for photos) and lossy WebP would require linking C libwebp across the 3-OS CI. JPEG via `image`'s pure-Rust `JpegEncoder` is small, lossy, zero-C-dep, and cross-platform. The `thumbnails.format` column stores `"jpeg"`, so WebP can replace it later with **no schema migration**. This is a documented, reversible deviation honoring the "minimal dependencies / CI on 3 OSes" constraints. (`image`/JPEG is already in the settled stack — Architecture map §2 export row.)
- **Pinned versions:** confirm the newest matching patch with `cargo add` during execution (same convention as Plans 1 & chrome). Critically, the `image` version **must unify with the one `rawler` re-exports** — after adding deps, run `cargo tree -i image` and align our `image` requirement to rawler's minor so `DynamicImage` types match. If a `rawler` 0.7.x signature differs from what is written below, adjust minimally and note it.
- **Files focused:** 200–400 lines/file target, 800 max. (User coding-style rule.)
- **Frequent commits:** one commit per task minimum; conventional-commit messages.
- **No `ferrolite-jobs`, no UI grid, no VT/viewer** in this plan (Plans 3–4). `decode_full` is built per the contract but is consumed later. Metadata is **read-only** (`little_exif` writes + XMP sidecars are Spec 2).

---

## File structure (this plan)

```
Cargo.toml                                  # add members, bump rust-version, add [workspace.dependencies]
fixtures/raw/                               # one small CC0 RAW fixture (shared by decode + catalog tests)
ferrolite-image/
  Cargo.toml
  src/
    lib.rs                                  # re-exports
    pixel.rs                                # PixelFormat, ImageBuffer (+ tests)
    orientation.rs                          # Orientation enum + from_exif + pure logic (+ tests)
ferrolite-decode/
  Cargo.toml
  src/
    lib.rs                                  # pub: decode_preview, decode_full, read_metadata; re-exports error/metadata
    error.rs                                # DecodeError (thiserror)
    metadata.rs                             # Metadata struct
    preview.rs                              # decode_preview + apply_orientation
    raw.rs                                  # decode_full + RawDecoded
  tests/
    decode.rs                               # integration tests against fixtures/raw
ferrolite-catalog/
  Cargo.toml
  src/
    lib.rs                                  # re-exports
    error.rs                                # CatalogError (thiserror)
    schema.rs                               # SCHEMA_VERSION + migrate()
    model.rs                                # NewImage, ImageRecord, DecodeStatus, IngestSummary, Thumbnail
    catalog.rs                              # Catalog: open, upsert_folder/image, queries, count
    thumbnail.rs                            # ThumbnailStore trait + Catalog impl + generate_thumbnail()
    ingest.rs                               # Catalog::ingest_folder (walk + incremental skip + parallel thumbnails)
  tests/
    catalog.rs                              # integration tests (temp DB + fixtures/raw)
```

---

### Task 1: Workspace wiring + `ferrolite-image` vocabulary crate

**Files:**
- Modify: `Cargo.toml` (members, `rust-version`, `[workspace.dependencies]`)
- Create: `ferrolite-image/Cargo.toml`, `ferrolite-image/src/lib.rs`, `ferrolite-image/src/pixel.rs`, `ferrolite-image/src/orientation.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `ferrolite_image::PixelFormat { Rgb8, Rgba8 }` (`Copy`, `Eq`) with `channels(self) -> usize`.
  - `ferrolite_image::ImageBuffer { width: u32, height: u32, format: PixelFormat, pixels: Vec<u8> }` with `expected_len(w,h,fmt) -> usize` and `new(w,h,fmt,pixels) -> Result<Self, ImageBufferError>`.
  - `ferrolite_image::ImageBufferError` (`Debug`, `Display`).
  - `ferrolite_image::Orientation` (8 EXIF variants, `Copy`, `Eq`, `Default = Normal`) with `from_exif(u16) -> Orientation`, `to_exif(self) -> u16`, `swaps_dimensions(self) -> bool`.

- [ ] **Step 1: Update the workspace manifest**

Rewrite `Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = ["ferrolite-app", "ferrolite-image", "ferrolite-decode", "ferrolite-catalog"]

[workspace.package]
edition = "2021"
license = "GPL-3.0-only"
rust-version = "1.88"

[workspace.dependencies]
ferrolite-image = { path = "ferrolite-image" }
ferrolite-decode = { path = "ferrolite-decode" }
rawler = "0.7.2"
rusqlite = { version = "0.40", features = ["bundled"] }
image = { version = "0.25", default-features = false }
fast_image_resize = "6.0"
rayon = "1.10"
walkdir = "2.5"
thiserror = "2.0"

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }
```
Note: `rust-version` bumped `1.85 → 1.88` for rawler 0.7.2. `image` carries `default-features = false` here; each consumer opts into the codecs it needs (`ferrolite-catalog` adds `"jpeg"`).

- [ ] **Step 2: Create the `ferrolite-image` manifest**

`ferrolite-image/Cargo.toml`:
```toml
[package]
name = "ferrolite-image"
version = "0.0.1"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
```
(Intentionally no dependencies — engine-transferable purity.)

- [ ] **Step 3: Write `pixel.rs` with failing tests first**

`ferrolite-image/src/pixel.rs`:
```rust
//! Pixel format + a validated interleaved-8-bit image buffer. Zero deps so this
//! vocabulary stays liftable into the engine-transferable tier.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb8,
    Rgba8,
}

impl PixelFormat {
    pub fn channels(self) -> usize {
        match self {
            PixelFormat::Rgb8 => 3,
            PixelFormat::Rgba8 => 4,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ImageBufferError {
    pub expected: usize,
    pub actual: usize,
}

impl fmt::Display for ImageBufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pixel buffer length {} does not match expected {}",
            self.actual, self.expected
        )
    }
}

impl std::error::Error for ImageBufferError {}

/// Interleaved 8-bit-per-channel image. `pixels.len() == width*height*channels`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBuffer {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub pixels: Vec<u8>,
}

impl ImageBuffer {
    pub fn expected_len(width: u32, height: u32, format: PixelFormat) -> usize {
        width as usize * height as usize * format.channels()
    }

    pub fn new(
        width: u32,
        height: u32,
        format: PixelFormat,
        pixels: Vec<u8>,
    ) -> Result<Self, ImageBufferError> {
        let expected = Self::expected_len(width, height, format);
        if pixels.len() != expected {
            return Err(ImageBufferError {
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            format,
            pixels,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_match_format() {
        assert_eq!(PixelFormat::Rgb8.channels(), 3);
        assert_eq!(PixelFormat::Rgba8.channels(), 4);
    }

    #[test]
    fn expected_len_multiplies_dimensions_and_channels() {
        assert_eq!(ImageBuffer::expected_len(4, 2, PixelFormat::Rgb8), 24);
        assert_eq!(ImageBuffer::expected_len(4, 2, PixelFormat::Rgba8), 32);
    }

    #[test]
    fn new_accepts_correct_length() {
        let buf = ImageBuffer::new(2, 1, PixelFormat::Rgb8, vec![0; 6]).unwrap();
        assert_eq!(buf.width, 2);
        assert_eq!(buf.pixels.len(), 6);
    }

    #[test]
    fn new_rejects_wrong_length() {
        let err = ImageBuffer::new(2, 1, PixelFormat::Rgb8, vec![0; 5]).unwrap_err();
        assert_eq!(err.expected, 6);
        assert_eq!(err.actual, 5);
    }
}
```

- [ ] **Step 4: Write `orientation.rs` with failing tests first**

`ferrolite-image/src/orientation.rs`:
```rust
//! EXIF orientation (tag values 1..=8) with pure mapping logic. The pixel
//! transform itself lives in the consumer (ferrolite-decode applies it via the
//! `image` crate); this enum is the shared, testable vocabulary.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    #[default]
    Normal, // 1
    FlipH,  // 2: mirror horizontal
    Rotate180, // 3
    FlipV,  // 4: mirror vertical
    Transpose, // 5: mirror across main diagonal
    Rotate90, // 6: 90° clockwise
    Transverse, // 7: mirror across anti-diagonal
    Rotate270, // 8: 270° clockwise
}

impl Orientation {
    /// Map an EXIF orientation tag value to the enum. Unknown/absent → `Normal`.
    pub fn from_exif(value: u16) -> Orientation {
        match value {
            2 => Orientation::FlipH,
            3 => Orientation::Rotate180,
            4 => Orientation::FlipV,
            5 => Orientation::Transpose,
            6 => Orientation::Rotate90,
            7 => Orientation::Transverse,
            8 => Orientation::Rotate270,
            _ => Orientation::Normal, // 1 and anything unexpected
        }
    }

    pub fn to_exif(self) -> u16 {
        match self {
            Orientation::Normal => 1,
            Orientation::FlipH => 2,
            Orientation::Rotate180 => 3,
            Orientation::FlipV => 4,
            Orientation::Transpose => 5,
            Orientation::Rotate90 => 6,
            Orientation::Transverse => 7,
            Orientation::Rotate270 => 8,
        }
    }

    /// True when applying the orientation swaps width and height.
    pub fn swaps_dimensions(self) -> bool {
        matches!(
            self,
            Orientation::Transpose
                | Orientation::Rotate90
                | Orientation::Transverse
                | Orientation::Rotate270
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_exif_maps_all_eight_values() {
        assert_eq!(Orientation::from_exif(1), Orientation::Normal);
        assert_eq!(Orientation::from_exif(2), Orientation::FlipH);
        assert_eq!(Orientation::from_exif(3), Orientation::Rotate180);
        assert_eq!(Orientation::from_exif(4), Orientation::FlipV);
        assert_eq!(Orientation::from_exif(5), Orientation::Transpose);
        assert_eq!(Orientation::from_exif(6), Orientation::Rotate90);
        assert_eq!(Orientation::from_exif(7), Orientation::Transverse);
        assert_eq!(Orientation::from_exif(8), Orientation::Rotate270);
    }

    #[test]
    fn from_exif_defaults_unknown_to_normal() {
        assert_eq!(Orientation::from_exif(0), Orientation::Normal);
        assert_eq!(Orientation::from_exif(99), Orientation::Normal);
    }

    #[test]
    fn to_exif_round_trips() {
        for v in 1..=8u16 {
            assert_eq!(Orientation::from_exif(v).to_exif(), v);
        }
    }

    #[test]
    fn swaps_dimensions_only_for_quarter_turns_and_diagonals() {
        assert!(!Orientation::Normal.swaps_dimensions());
        assert!(!Orientation::Rotate180.swaps_dimensions());
        assert!(Orientation::Rotate90.swaps_dimensions());
        assert!(Orientation::Rotate270.swaps_dimensions());
        assert!(Orientation::Transpose.swaps_dimensions());
        assert!(Orientation::Transverse.swaps_dimensions());
    }

    #[test]
    fn default_is_normal() {
        assert_eq!(Orientation::default(), Orientation::Normal);
    }
}
```

- [ ] **Step 5: Write the crate root**

`ferrolite-image/src/lib.rs`:
```rust
//! Core pixel/orientation vocabulary shared across ferrolite crates.

mod orientation;
mod pixel;

pub use orientation::Orientation;
pub use pixel::{ImageBuffer, ImageBufferError, PixelFormat};
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p ferrolite-image`
Expected: PASS (pixel: 4 tests, orientation: 6 tests = 10). Then `cargo clippy -p ferrolite-image --all-targets -- -D warnings` is clean.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml ferrolite-image
git commit -m "feat(image): pixel/orientation vocabulary crate; bump workspace MSRV to 1.88"
```

---

### Task 2: `ferrolite-decode` — error, metadata, and `read_metadata` (+ fixture)

**Files:**
- Create: `ferrolite-decode/Cargo.toml`, `ferrolite-decode/src/lib.rs`, `ferrolite-decode/src/error.rs`, `ferrolite-decode/src/metadata.rs`, `ferrolite-decode/tests/decode.rs`
- Create: `fixtures/raw/<sample>` (one small CC0 RAW)

**Interfaces:**
- Consumes: `ferrolite_image::Orientation`; `rawler::{decode helpers}`.
- Produces:
  - `ferrolite_decode::DecodeError` (`thiserror`, `Debug + Display + Error`).
  - `ferrolite_decode::Metadata { make, model, width, height, orientation, iso, aperture, shutter, focal_length, capture_time, lens }`.
  - `ferrolite_decode::read_metadata(path: &std::path::Path) -> Result<Metadata, DecodeError>`.

- [ ] **Step 1: Create the manifest**

`ferrolite-decode/Cargo.toml`:
```toml
[package]
name = "ferrolite-decode"
version = "0.0.1"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
ferrolite-image = { workspace = true }
rawler = { workspace = true }
image = { workspace = true }
thiserror = { workspace = true }
```

- [ ] **Step 2: Add the test fixture**

Download one **small (< ~15 MB) CC0 RAW** into `fixtures/raw/` (raw.pixls.us files are CC0; prefer an older/smaller-sensor camera so the committed binary stays small — e.g. a Panasonic/Olympus or older Canon body). Save it with its natural extension, e.g. `fixtures/raw/sample.rw2`. Then verify rawler can open it:
```bash
mkdir -p fixtures/raw
# (download your chosen CC0 RAW to fixtures/raw/sample.<ext>)
ls -la fixtures/raw
```
The tests below are **extension-agnostic** (they pick the first file in `fixtures/raw/`), so any valid RAW that rawler 0.7.2 supports works. If the file is larger than you want in git history, add it via `git lfs track 'fixtures/raw/*'` first; otherwise commit it directly.

- [ ] **Step 3: Write the error type**

`ferrolite-decode/src/error.rs`:
```rust
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("rawler error: {0}")]
    Rawler(String),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("no embedded preview, full image, or thumbnail in {0}")]
    NoPreview(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// rawler's error type implements `Display`; we flatten it to a string so this
/// crate does not re-export rawler's error in its public API.
pub(crate) fn rawler<E: std::fmt::Display>(e: E) -> DecodeError {
    DecodeError::Rawler(e.to_string())
}
```

- [ ] **Step 4: Write the metadata module**

`ferrolite-decode/src/metadata.rs`:
```rust
use ferrolite_image::Orientation;

/// Camera/exposure metadata read cheaply from a RAW (no full pixel decode).
#[derive(Debug, Clone, PartialEq)]
pub struct Metadata {
    pub make: String,
    pub model: String,
    pub width: u32,
    pub height: u32,
    pub orientation: Orientation,
    pub iso: Option<u32>,
    pub aperture: Option<f32>,
    pub shutter: Option<f32>,
    pub focal_length: Option<f32>,
    pub capture_time: Option<String>,
    pub lens: Option<String>,
}
```

- [ ] **Step 5: Write the failing integration test**

`ferrolite-decode/tests/decode.rs`:
```rust
use std::path::{Path, PathBuf};

/// First file in the shared fixture directory (extension-agnostic).
fn fixture() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/raw");
    std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.is_file())
        .expect("a RAW fixture in fixtures/raw")
}

#[test]
fn read_metadata_returns_camera_and_dimensions() {
    let meta = ferrolite_decode::read_metadata(&fixture()).expect("metadata");
    assert!(!meta.make.is_empty(), "make should be populated");
    assert!(!meta.model.is_empty(), "model should be populated");
    assert!(meta.width > 0 && meta.height > 0, "dimensions should be > 0");
}
```

- [ ] **Step 6: Run it to verify it fails**

Run: `cargo test -p ferrolite-decode read_metadata`
Expected: FAIL (compile error — `read_metadata` not defined).

- [ ] **Step 7: Implement `read_metadata` and the crate root**

`ferrolite-decode/src/lib.rs`:
```rust
//! RAW decode: the three independently-consumable products (preview, full,
//! metadata) the two-tier load path relies on. Wraps `rawler` 0.7.x.

mod error;
mod metadata;

pub use error::DecodeError;
pub use metadata::Metadata;

use ferrolite_image::Orientation;
use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;
use std::path::Path;

use crate::error::rawler as rawler_err;

/// rawler `Rational`/`SRational` → f32. NOTE: confirm the field accessors against
/// rawler 0.7.2 (`rawler::exif::Rational`); if the fields are named differently
/// (e.g. `num`/`den` vs `n`/`d`, or `.as_f32()` exists), adjust this one helper.
fn rat(num: i64, den: i64) -> Option<f32> {
    if den == 0 {
        None
    } else {
        Some(num as f32 / den as f32)
    }
}

/// Read camera/exposure metadata and image dimensions without decoding pixels.
/// Dimensions come from a `dummy` raw_image call (fills geometry, skips pixels).
pub fn read_metadata(path: &Path) -> Result<Metadata, DecodeError> {
    let src = RawSource::new(path).map_err(rawler_err)?;
    let decoder = rawler::get_decoder(&src).map_err(rawler_err)?;
    let params = RawDecodeParams::default();

    let meta = decoder.raw_metadata(&src, &params).map_err(rawler_err)?;
    // `dummy = true`: geometry only, no pixel decode (fast).
    let dims = decoder.raw_image(&src, &params, true).map_err(rawler_err)?;

    let e = &meta.exif;
    Ok(Metadata {
        make: meta.make.clone(),
        model: meta.model.clone(),
        width: dims.width as u32,
        height: dims.height as u32,
        orientation: Orientation::from_exif(e.orientation.unwrap_or(1)),
        iso: e.iso_speed_ratings.map(u32::from),
        // NOTE: rawler exposes these as Rational/SRational. The `.num`/`.den`
        // field access below tracks rawler 0.7.2; adjust to the resolved struct
        // (see `rat` helper note) if the compiler reports different field names.
        aperture: e.fnumber.as_ref().and_then(|r| rat(r.num as i64, r.den as i64)),
        shutter: e
            .exposure_time
            .as_ref()
            .and_then(|r| rat(r.num as i64, r.den as i64)),
        focal_length: e
            .focal_length
            .as_ref()
            .and_then(|r| rat(r.num as i64, r.den as i64)),
        capture_time: e.date_time_original.clone(),
        lens: e.lens_model.clone(),
    })
}
```
Note (verified against docs.rs/rawler/0.7.2): `RawSource::new(&Path)`, `rawler::get_decoder(&RawSource)`, `Decoder::raw_metadata(&RawSource, &RawDecodeParams)`, `Decoder::raw_image(&RawSource, &RawDecodeParams, dummy: bool)`, `RawMetadata { make, model, exif }`, and `Exif { orientation: Option<u16>, fnumber, exposure_time, focal_length, iso_speed_ratings: Option<u16>, date_time_original: Option<String>, lens_model: Option<String> }`. The only unconfirmed surface is the `Rational` field names — isolated to the `rat`/`.num`/`.den` lines with a note.

- [ ] **Step 8: Run the test**

Run: `cargo test -p ferrolite-decode read_metadata`
Expected: PASS. Then `cargo clippy -p ferrolite-decode --all-targets -- -D warnings` clean.

- [ ] **Step 9: Commit**

```bash
git add ferrolite-decode Cargo.toml fixtures
git commit -m "feat(decode): read_metadata via rawler + CC0 RAW test fixture"
```

---

### Task 3: `ferrolite-decode` — `decode_preview` (oriented RGB8)

**Files:**
- Create: `ferrolite-decode/src/preview.rs`
- Modify: `ferrolite-decode/src/lib.rs` (add `mod preview; pub use preview::decode_preview;`), `ferrolite-decode/tests/decode.rs` (add test)

**Interfaces:**
- Consumes: `ferrolite_image::{ImageBuffer, PixelFormat, Orientation}`; `image::DynamicImage`; `read_metadata`'s rawler calls.
- Produces: `ferrolite_decode::decode_preview(path: &Path) -> Result<ImageBuffer, DecodeError>` — an **upright RGB8** preview (orientation already applied), ready for thumbnailing.

- [ ] **Step 1: Write the failing test**

Append to `ferrolite-decode/tests/decode.rs`:
```rust
#[test]
fn decode_preview_returns_nonempty_rgb8() {
    use ferrolite_image::PixelFormat;
    let buf = ferrolite_decode::decode_preview(&fixture()).expect("preview");
    assert_eq!(buf.format, PixelFormat::Rgb8);
    assert!(buf.width > 0 && buf.height > 0);
    assert_eq!(
        buf.pixels.len(),
        buf.width as usize * buf.height as usize * 3
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p ferrolite-decode decode_preview`
Expected: FAIL (compile error — `decode_preview` not defined).

- [ ] **Step 3: Implement `preview.rs`**

`ferrolite-decode/src/preview.rs`:
```rust
use crate::error::{rawler as rawler_err, DecodeError};
use ferrolite_image::{ImageBuffer, Orientation, PixelFormat};
use image::DynamicImage;
use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;
use std::path::Path;

/// Decode an upright RGB8 preview. Tries the embedded preview JPEG, then the
/// embedded full-size JPEG, then the embedded thumbnail — first one present
/// wins. Orientation from EXIF is applied so the result is display-upright.
pub fn decode_preview(path: &Path) -> Result<ImageBuffer, DecodeError> {
    let src = RawSource::new(path).map_err(rawler_err)?;
    let decoder = rawler::get_decoder(&src).map_err(rawler_err)?;
    let params = RawDecodeParams::default();

    let dynimg = decoder
        .preview_image(&src, &params)
        .map_err(rawler_err)?
        .or_else(|| decoder.full_image(&src, &params).ok().flatten())
        .or_else(|| decoder.thumbnail_image(&src, &params).ok().flatten())
        .ok_or_else(|| DecodeError::NoPreview(path.to_path_buf()))?;

    let exif_orientation = decoder
        .raw_metadata(&src, &params)
        .map_err(rawler_err)?
        .exif
        .orientation
        .unwrap_or(1);
    let oriented = apply_orientation(dynimg, Orientation::from_exif(exif_orientation));

    let rgb = oriented.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    ImageBuffer::new(w, h, PixelFormat::Rgb8, rgb.into_raw())
        .expect("RGB8 buffer length is w*h*3 by construction")
}

/// Apply an EXIF orientation to a decoded image using the `image` crate's
/// rotate/flip ops. (rotate90/270 are clockwise in the `image` crate.)
fn apply_orientation(img: DynamicImage, o: Orientation) -> DynamicImage {
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

- [ ] **Step 4: Wire it into the crate root**

In `ferrolite-decode/src/lib.rs`, add `mod preview;` next to the other modules and `pub use preview::decode_preview;` next to the other re-exports.

- [ ] **Step 5: Run the test**

Run: `cargo test -p ferrolite-decode decode_preview`
Expected: PASS. Then `cargo clippy -p ferrolite-decode --all-targets -- -D warnings` clean.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-decode/src/preview.rs ferrolite-decode/src/lib.rs ferrolite-decode/tests/decode.rs
git commit -m "feat(decode): decode_preview — upright RGB8 from embedded JPEG"
```

---

### Task 4: `ferrolite-decode` — `decode_full` (RawDecoded)

**Files:**
- Create: `ferrolite-decode/src/raw.rs`
- Modify: `ferrolite-decode/src/lib.rs` (add `mod raw; pub use raw::{decode_full, RawDecoded};`), `ferrolite-decode/tests/decode.rs` (add test)

**Interfaces:**
- Consumes: rawler full decode (`raw_image(.., dummy=false)`).
- Produces:
  - `ferrolite_decode::RawDecoded { width: u32, height: u32, cpp: usize, pixels: Vec<u16> }`.
  - `ferrolite_decode::decode_full(path: &Path) -> Result<RawDecoded, DecodeError>`.
- This product is consumed by Plan 4 (VT source); Plan 2 only verifies it decodes.

- [ ] **Step 1: Write the failing test**

Append to `ferrolite-decode/tests/decode.rs`:
```rust
#[test]
fn decode_full_matches_metadata_dimensions_and_buffer() {
    let meta = ferrolite_decode::read_metadata(&fixture()).expect("metadata");
    let full = ferrolite_decode::decode_full(&fixture()).expect("full decode");
    assert_eq!(full.width, meta.width);
    assert_eq!(full.height, meta.height);
    assert!(full.cpp >= 1);
    assert_eq!(
        full.pixels.len(),
        full.width as usize * full.height as usize * full.cpp
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p ferrolite-decode decode_full`
Expected: FAIL (compile error — `decode_full` not defined).

- [ ] **Step 3: Implement `raw.rs`**

`ferrolite-decode/src/raw.rs`:
```rust
use crate::error::{rawler as rawler_err, DecodeError};
use rawler::decoders::RawDecodeParams;
use rawler::rawimage::RawImageData;
use rawler::rawsource::RawSource;
use std::path::Path;

/// A fully decoded RAW: integer CFA/sensor samples plus geometry. Consumed by
/// the VT/viewer in a later plan; here it only proves rawler decodes the file.
#[derive(Debug, Clone)]
pub struct RawDecoded {
    pub width: u32,
    pub height: u32,
    /// Components per pixel (1 for Bayer CFA, 3/4 for some formats).
    pub cpp: usize,
    /// Sensor samples, length `width * height * cpp`.
    pub pixels: Vec<u16>,
}

pub fn decode_full(path: &Path) -> Result<RawDecoded, DecodeError> {
    let src = RawSource::new(path).map_err(rawler_err)?;
    let decoder = rawler::get_decoder(&src).map_err(rawler_err)?;
    let params = RawDecodeParams::default();
    let img = decoder.raw_image(&src, &params, false).map_err(rawler_err)?;

    // RawImageData is Integer(Vec<u16>) for almost all formats; a few DNGs are
    // Float — quantize to u16 for this plan's display-only consumer.
    let pixels = match img.data {
        RawImageData::Integer(v) => v,
        RawImageData::Float(v) => v.iter().map(|f| f.round().clamp(0.0, 65535.0) as u16).collect(),
    };

    Ok(RawDecoded {
        width: img.width as u32,
        height: img.height as u32,
        cpp: img.cpp,
        pixels,
    })
}
```
Note: `RawImage { width: usize, height: usize, cpp: usize, data: RawImageData }` and `RawImageData::{Integer(Vec<u16>), Float(Vec<f32>)}` are verified against docs.rs/rawler/0.7.2. If `img.cpp` is named differently on the resolved patch, adjust minimally.

- [ ] **Step 4: Wire it into the crate root**

In `ferrolite-decode/src/lib.rs`, add `mod raw;` and `pub use raw::{decode_full, RawDecoded};`.

- [ ] **Step 5: Run the test**

Run: `cargo test -p ferrolite-decode decode_full`
Expected: PASS. Then `cargo test -p ferrolite-decode` (all 4 integration tests) and `cargo clippy -p ferrolite-decode --all-targets -- -D warnings` clean.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-decode/src/raw.rs ferrolite-decode/src/lib.rs ferrolite-decode/tests/decode.rs
git commit -m "feat(decode): decode_full → RawDecoded (full-res sensor samples)"
```

---

### Task 5: `ferrolite-catalog` — schema + `user_version` migrations

**Files:**
- Create: `ferrolite-catalog/Cargo.toml`, `ferrolite-catalog/src/lib.rs`, `ferrolite-catalog/src/error.rs`, `ferrolite-catalog/src/schema.rs`, `ferrolite-catalog/src/catalog.rs`, `ferrolite-catalog/tests/catalog.rs`

**Interfaces:**
- Consumes: `rusqlite::Connection`.
- Produces:
  - `ferrolite_catalog::CatalogError` (`thiserror`).
  - `ferrolite_catalog::schema::{SCHEMA_VERSION, migrate}` (crate-internal `pub(crate)`).
  - `ferrolite_catalog::Catalog` with `open(path) -> Result<Self>`, `open_in_memory() -> Result<Self>`, and `pub(crate) conn(&self) -> &Connection`.

- [ ] **Step 1: Create the manifest**

`ferrolite-catalog/Cargo.toml`:
```toml
[package]
name = "ferrolite-catalog"
version = "0.0.1"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
ferrolite-image = { workspace = true }
ferrolite-decode = { workspace = true }
rusqlite = { workspace = true }
image = { workspace = true, features = ["jpeg"] }
fast_image_resize = { workspace = true }
rayon = { workspace = true }
walkdir = { workspace = true }
thiserror = { workspace = true }
```

- [ ] **Step 2: Write the error type**

`ferrolite-catalog/src/error.rs`:
```rust
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("decode error: {0}")]
    Decode(#[from] ferrolite_decode::DecodeError),
    #[error("thumbnail encode error: {0}")]
    Encode(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
```

- [ ] **Step 3: Write the failing migration test**

`ferrolite-catalog/tests/catalog.rs`:
```rust
use ferrolite_catalog::Catalog;

#[test]
fn fresh_db_is_migrated_to_current_version() {
    let cat = Catalog::open_in_memory().expect("open");
    assert_eq!(cat.schema_version().expect("version"), ferrolite_catalog::SCHEMA_VERSION);
}

#[test]
fn migrate_is_idempotent_on_reopen() {
    let dir = tempdir();
    let path = dir.join("catalog.db");
    {
        let _ = Catalog::open(&path).expect("first open");
    }
    // Reopening an already-migrated DB must not error or downgrade.
    let cat = Catalog::open(&path).expect("second open");
    assert_eq!(cat.schema_version().expect("version"), ferrolite_catalog::SCHEMA_VERSION);
}

/// Minimal temp dir without an extra dependency: unique path under the OS temp
/// dir using the test thread name + a process-unique counter.
fn tempdir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("ferrolite-cat-{pid}-{n}"));
    std::fs::create_dir_all(&dir).expect("mkdir temp");
    dir
}
```
Note: tests share this `tempdir()` helper; later tasks add more tests to this file and reuse it.

- [ ] **Step 4: Run it to verify it fails**

Run: `cargo test -p ferrolite-catalog migrated`
Expected: FAIL (compile error — `Catalog`/`SCHEMA_VERSION` not defined).

- [ ] **Step 5: Write the schema/migrations module**

`ferrolite-catalog/src/schema.rs`:
```rust
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

    conn.pragma_update(None, "user_version", version)?;
    Ok(())
}
```

- [ ] **Step 6: Write the `Catalog` type + crate root**

`ferrolite-catalog/src/catalog.rs`:
```rust
use crate::error::CatalogError;
use crate::schema;
use rusqlite::Connection;
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
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, CatalogError> {
        let conn = Connection::open_in_memory()?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn schema_version(&self) -> Result<i64, CatalogError> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}
```

`ferrolite-catalog/src/lib.rs`:
```rust
//! SQLite digital-asset-management catalog: schema, ingest, thumbnails, queries.

mod catalog;
mod error;
mod schema;

pub use catalog::Catalog;
pub use error::CatalogError;
pub use schema::SCHEMA_VERSION;
```

- [ ] **Step 7: Run the tests**

Run: `cargo test -p ferrolite-catalog migrated` then `cargo test -p ferrolite-catalog idempotent`
Expected: PASS (2 tests). Then `cargo clippy -p ferrolite-catalog --all-targets -- -D warnings` clean.

- [ ] **Step 8: Commit**

```bash
git add ferrolite-catalog
git commit -m "feat(catalog): SQLite schema + user_version migrations"
```

---

### Task 6: `ferrolite-catalog` — image/folder upsert, queries, count

**Files:**
- Create: `ferrolite-catalog/src/model.rs`
- Modify: `ferrolite-catalog/src/catalog.rs` (add CRUD methods), `ferrolite-catalog/src/lib.rs` (re-export model types), `ferrolite-catalog/tests/catalog.rs` (add tests)

**Interfaces:**
- Consumes: `Catalog::conn()`; `ferrolite_image::Orientation`.
- Produces:
  - `ferrolite_catalog::DecodeStatus { Pending=0, Done=1, Failed=2 }` (`Copy`, `Eq`) with `as_i64`/`from_i64`.
  - `ferrolite_catalog::NewImage { folder_id, filename, mtime, size, make, model, width, height, orientation, capture_time, iso, decode_status }`.
  - `ferrolite_catalog::ImageRecord { id, folder_id, filename, width, height, orientation, capture_time, iso, decode_status }`.
  - On `Catalog`: `upsert_folder(&self, path:&Path) -> Result<i64>`, `upsert_image(&self, img:&NewImage) -> Result<i64>`, `image_by_name(&self, folder_id:i64, filename:&str) -> Result<Option<ImageRecord>>`, `list_images(&self, folder_id:i64) -> Result<Vec<ImageRecord>>`, `image_count(&self) -> Result<u64>`, `needs_reingest(&self, folder_id:i64, filename:&str, mtime:i64, size:i64) -> Result<bool>`.

- [ ] **Step 1: Write the failing tests**

Append to `ferrolite-catalog/tests/catalog.rs`:
```rust
use ferrolite_catalog::{DecodeStatus, NewImage};
use ferrolite_image::Orientation;

fn sample_image(folder_id: i64, filename: &str) -> NewImage {
    NewImage {
        folder_id,
        filename: filename.to_string(),
        mtime: 1000,
        size: 2000,
        make: Some("Nikon".into()),
        model: Some("Z f".into()),
        width: Some(6048),
        height: Some(4032),
        orientation: Orientation::Rotate90,
        capture_time: Some("2026:06:29 12:00:00".into()),
        iso: Some(100),
        decode_status: DecodeStatus::Done,
    }
}

#[test]
fn upsert_and_query_round_trip() {
    let cat = Catalog::open_in_memory().unwrap();
    let folder = cat.upsert_folder(std::path::Path::new("/photos/a")).unwrap();
    let id = cat.upsert_image(&sample_image(folder, "DSC_0001.NEF")).unwrap();

    let rec = cat.image_by_name(folder, "DSC_0001.NEF").unwrap().expect("row");
    assert_eq!(rec.id, id);
    assert_eq!(rec.width, Some(6048));
    assert_eq!(rec.orientation, Orientation::Rotate90);
    assert_eq!(rec.decode_status, DecodeStatus::Done);

    assert_eq!(cat.list_images(folder).unwrap().len(), 1);
    assert_eq!(cat.image_count().unwrap(), 1);
}

#[test]
fn upsert_is_idempotent_on_folder_and_filename() {
    let cat = Catalog::open_in_memory().unwrap();
    let folder = cat.upsert_folder(std::path::Path::new("/photos/a")).unwrap();
    assert_eq!(folder, cat.upsert_folder(std::path::Path::new("/photos/a")).unwrap());

    let first = cat.upsert_image(&sample_image(folder, "DSC_0001.NEF")).unwrap();
    let second = cat.upsert_image(&sample_image(folder, "DSC_0001.NEF")).unwrap();
    assert_eq!(first, second, "same (folder, filename) updates the same row");
    assert_eq!(cat.image_count().unwrap(), 1);
}

#[test]
fn needs_reingest_detects_changes() {
    let cat = Catalog::open_in_memory().unwrap();
    let folder = cat.upsert_folder(std::path::Path::new("/photos/a")).unwrap();
    cat.upsert_image(&sample_image(folder, "DSC_0001.NEF")).unwrap();

    assert!(!cat.needs_reingest(folder, "DSC_0001.NEF", 1000, 2000).unwrap(), "unchanged");
    assert!(cat.needs_reingest(folder, "DSC_0001.NEF", 1001, 2000).unwrap(), "mtime changed");
    assert!(cat.needs_reingest(folder, "DSC_0001.NEF", 1000, 9999).unwrap(), "size changed");
    assert!(cat.needs_reingest(folder, "NEW.NEF", 1, 1).unwrap(), "new file");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-catalog round_trip`
Expected: FAIL (compile error — model types/methods not defined).

- [ ] **Step 3: Write `model.rs`**

`ferrolite-catalog/src/model.rs`:
```rust
use ferrolite_image::Orientation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeStatus {
    Pending,
    Done,
    Failed,
}

impl DecodeStatus {
    pub fn as_i64(self) -> i64 {
        match self {
            DecodeStatus::Pending => 0,
            DecodeStatus::Done => 1,
            DecodeStatus::Failed => 2,
        }
    }

    pub fn from_i64(v: i64) -> DecodeStatus {
        match v {
            1 => DecodeStatus::Done,
            2 => DecodeStatus::Failed,
            _ => DecodeStatus::Pending,
        }
    }
}

/// Values written when ingesting one image.
#[derive(Debug, Clone)]
pub struct NewImage {
    pub folder_id: i64,
    pub filename: String,
    pub mtime: i64,
    pub size: i64,
    pub make: Option<String>,
    pub model: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Orientation,
    pub capture_time: Option<String>,
    pub iso: Option<u32>,
    pub decode_status: DecodeStatus,
}

/// Row read back from the catalog for the grid/status bar.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRecord {
    pub id: i64,
    pub folder_id: i64,
    pub filename: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Orientation,
    pub capture_time: Option<String>,
    pub iso: Option<u32>,
    pub decode_status: DecodeStatus,
}

/// Result of an ingest pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IngestSummary {
    pub scanned: usize,
    pub added: usize,
    pub skipped: usize,
    pub failed: usize,
}
```

- [ ] **Step 4: Add the CRUD methods to `Catalog`**

Append to the `impl Catalog` block in `ferrolite-catalog/src/catalog.rs` (and add `use crate::model::{DecodeStatus, ImageRecord, NewImage};` plus `use ferrolite_image::Orientation;` to the file's imports):
```rust
    /// Insert a folder by path, or return the existing id. Idempotent.
    pub fn upsert_folder(&self, path: &Path) -> Result<i64, CatalogError> {
        let p = path.to_string_lossy();
        self.conn.execute(
            "INSERT INTO folders (path) VALUES (?1) ON CONFLICT(path) DO NOTHING",
            rusqlite::params![p],
        )?;
        let id = self.conn.query_row(
            "SELECT id FROM folders WHERE path = ?1",
            rusqlite::params![p],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// Insert or update an image keyed by (folder_id, filename). Returns its id.
    pub fn upsert_image(&self, img: &NewImage) -> Result<i64, CatalogError> {
        self.conn.execute(
            "INSERT INTO images
               (folder_id, filename, mtime, size, camera_make, camera_model,
                width, height, orientation, capture_time, iso, decode_status)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(folder_id, filename) DO UPDATE SET
                mtime=?3, size=?4, camera_make=?5, camera_model=?6, width=?7,
                height=?8, orientation=?9, capture_time=?10, iso=?11, decode_status=?12",
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
            ],
        )?;
        let id = self.conn.query_row(
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
        let mut stmt = self.conn.prepare(
            "SELECT id, folder_id, filename, width, height, orientation,
                    capture_time, iso, decode_status
             FROM images WHERE folder_id = ?1 AND filename = ?2",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![folder_id, filename], row_to_record)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn list_images(&self, folder_id: i64) -> Result<Vec<ImageRecord>, CatalogError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, folder_id, filename, width, height, orientation,
                    capture_time, iso, decode_status
             FROM images WHERE folder_id = ?1 ORDER BY filename",
        )?;
        let rows = stmt.query_map(rusqlite::params![folder_id], row_to_record)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn image_count(&self) -> Result<u64, CatalogError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM images", [], |row| row.get(0))?;
        Ok(n as u64)
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
        let existing: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT mtime, size FROM images WHERE folder_id = ?1 AND filename = ?2",
                rusqlite::params![folder_id, filename],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        Ok(match existing {
            Some((m, s)) => m != mtime || s != size,
            None => true,
        })
    }
```

Add this free function at the bottom of `ferrolite-catalog/src/catalog.rs`:
```rust
fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImageRecord> {
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
```

- [ ] **Step 5: Re-export model types**

In `ferrolite-catalog/src/lib.rs`, add `mod model;` and `pub use model::{DecodeStatus, ImageRecord, IngestSummary, NewImage};`.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p ferrolite-catalog`
Expected: PASS (migration 2 + round_trip + idempotent + needs_reingest = 5). Then `cargo clippy -p ferrolite-catalog --all-targets -- -D warnings` clean.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-catalog/src/model.rs ferrolite-catalog/src/catalog.rs ferrolite-catalog/src/lib.rs ferrolite-catalog/tests/catalog.rs
git commit -m "feat(catalog): folder/image upsert, queries, count, incremental skip"
```

---

### Task 7: `ferrolite-catalog` — `ThumbnailStore` + `generate_thumbnail`

**Files:**
- Create: `ferrolite-catalog/src/thumbnail.rs`
- Modify: `ferrolite-catalog/src/lib.rs` (add `mod thumbnail;` + re-exports), `ferrolite-catalog/tests/catalog.rs` (add tests)

**Interfaces:**
- Consumes: `ferrolite_image::{ImageBuffer, PixelFormat}`; `fast_image_resize`; `image::codecs::jpeg::JpegEncoder`; `Catalog::conn()`.
- Produces:
  - `ferrolite_catalog::Thumbnail { width: u32, height: u32, format: String, bytes: Vec<u8> }`.
  - `ferrolite_catalog::generate_thumbnail(preview: &ImageBuffer) -> Result<Thumbnail, CatalogError>` — resize to ≤256px, JPEG q85.
  - `ferrolite_catalog::ThumbnailStore` trait `{ put_thumbnail(&self, image_id, &Thumbnail) -> Result<()>; get_thumbnail(&self, image_id) -> Result<Option<Thumbnail>> }`, implemented for `Catalog`.
  - constants `THUMB_MAX_EDGE: u32 = 256`, `THUMB_QUALITY: u8 = 85`.

- [ ] **Step 1: Write the failing tests**

Append to `ferrolite-catalog/tests/catalog.rs`:
```rust
use ferrolite_catalog::{generate_thumbnail, Thumbnail, ThumbnailStore, THUMB_MAX_EDGE};
use ferrolite_image::{ImageBuffer, PixelFormat};

fn solid_rgb(width: u32, height: u32) -> ImageBuffer {
    let pixels = vec![120u8; (width * height * 3) as usize];
    ImageBuffer::new(width, height, PixelFormat::Rgb8, pixels).unwrap()
}

#[test]
fn generate_thumbnail_fits_within_max_edge_and_is_decodable_jpeg() {
    let thumb = generate_thumbnail(&solid_rgb(1024, 512)).expect("thumb");
    assert!(thumb.width <= THUMB_MAX_EDGE && thumb.height <= THUMB_MAX_EDGE);
    assert_eq!(thumb.format, "jpeg");
    // Aspect ratio preserved: 2:1 source → wider than tall.
    assert!(thumb.width > thumb.height);
    // Bytes decode as a JPEG of the reported size.
    let decoded = image::load_from_memory(&thumb.bytes).expect("decodes").to_rgb8();
    assert_eq!(decoded.width(), thumb.width);
    assert_eq!(decoded.height(), thumb.height);
}

#[test]
fn thumbnail_store_blob_round_trip() {
    let cat = Catalog::open_in_memory().unwrap();
    let folder = cat.upsert_folder(std::path::Path::new("/photos/a")).unwrap();
    let id = cat.upsert_image(&sample_image(folder, "DSC_0001.NEF")).unwrap();

    let thumb = generate_thumbnail(&solid_rgb(640, 480)).unwrap();
    cat.put_thumbnail(id, &thumb).unwrap();

    let got = cat.get_thumbnail(id).unwrap().expect("stored thumb");
    assert_eq!(got.width, thumb.width);
    assert_eq!(got.height, thumb.height);
    assert_eq!(got.format, "jpeg");
    assert_eq!(got.bytes, thumb.bytes);
    assert!(cat.get_thumbnail(999_999).unwrap().is_none(), "missing → None");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-catalog thumbnail`
Expected: FAIL (compile error — thumbnail items not defined).

- [ ] **Step 3: Write `thumbnail.rs`**

`ferrolite-catalog/src/thumbnail.rs`:
```rust
use crate::catalog::Catalog;
use crate::error::CatalogError;
use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use ferrolite_image::{ImageBuffer, PixelFormat};
use image::codecs::jpeg::JpegEncoder;
use image::ExtendedColorType;

pub const THUMB_MAX_EDGE: u32 = 256;
pub const THUMB_QUALITY: u8 = 85;
const THUMB_LEVEL: i64 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub bytes: Vec<u8>,
}

/// Storage for thumbnail blobs. A trait so a memory-mapped mipmap cache can
/// replace the SQLite-BLOB impl later with zero call-site change (design §4).
pub trait ThumbnailStore {
    fn put_thumbnail(&self, image_id: i64, thumb: &Thumbnail) -> Result<(), CatalogError>;
    fn get_thumbnail(&self, image_id: i64) -> Result<Option<Thumbnail>, CatalogError>;
}

/// Resize an RGB8 preview to fit within `THUMB_MAX_EDGE` (aspect preserved,
/// never upscaled) and encode it as JPEG q85.
pub fn generate_thumbnail(preview: &ImageBuffer) -> Result<Thumbnail, CatalogError> {
    // JPEG has no alpha; drop it if the source is RGBA.
    let (rgb, src_w, src_h) = to_rgb8(preview);

    let scale = (THUMB_MAX_EDGE as f32 / src_w as f32)
        .min(THUMB_MAX_EDGE as f32 / src_h as f32)
        .min(1.0);
    let dst_w = ((src_w as f32 * scale).round() as u32).max(1);
    let dst_h = ((src_h as f32 * scale).round() as u32).max(1);

    let src_img = Image::from_vec_u8(src_w, src_h, rgb, PixelType::U8x3)
        .map_err(|e| CatalogError::Encode(e.to_string()))?;
    let mut dst_img = Image::new(dst_w, dst_h, PixelType::U8x3);
    let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    Resizer::new()
        .resize(&src_img, &mut dst_img, &opts)
        .map_err(|e| CatalogError::Encode(e.to_string()))?;

    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, THUMB_QUALITY)
        .encode(dst_img.buffer(), dst_w, dst_h, ExtendedColorType::Rgb8)
        .map_err(|e| CatalogError::Encode(e.to_string()))?;

    Ok(Thumbnail {
        width: dst_w,
        height: dst_h,
        format: "jpeg".to_string(),
        bytes,
    })
}

/// Return tightly-packed RGB8 bytes plus dimensions, dropping alpha if present.
fn to_rgb8(buf: &ImageBuffer) -> (Vec<u8>, u32, u32) {
    match buf.format {
        PixelFormat::Rgb8 => (buf.pixels.clone(), buf.width, buf.height),
        PixelFormat::Rgba8 => {
            let mut rgb = Vec::with_capacity(buf.pixels.len() / 4 * 3);
            for px in buf.pixels.chunks_exact(4) {
                rgb.extend_from_slice(&px[0..3]);
            }
            (rgb, buf.width, buf.height)
        }
    }
}

impl ThumbnailStore for Catalog {
    fn put_thumbnail(&self, image_id: i64, thumb: &Thumbnail) -> Result<(), CatalogError> {
        self.conn().execute(
            "INSERT INTO thumbnails (image_id, level, w, h, format, blob)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(image_id) DO UPDATE SET
                level=?2, w=?3, h=?4, format=?5, blob=?6",
            rusqlite::params![
                image_id,
                THUMB_LEVEL,
                thumb.width as i64,
                thumb.height as i64,
                thumb.format,
                thumb.bytes,
            ],
        )?;
        Ok(())
    }

    fn get_thumbnail(&self, image_id: i64) -> Result<Option<Thumbnail>, CatalogError> {
        let mut stmt = self
            .conn()
            .prepare("SELECT w, h, format, blob FROM thumbnails WHERE image_id = ?1")?;
        let mut rows = stmt.query_map(rusqlite::params![image_id], |row| {
            Ok(Thumbnail {
                width: row.get::<_, i64>(0)? as u32,
                height: row.get::<_, i64>(1)? as u32,
                format: row.get(2)?,
                bytes: row.get(3)?,
            })
        })?;
        match rows.next() {
            Some(t) => Ok(Some(t?)),
            None => Ok(None),
        }
    }
}
```
Note (verified): `fast_image_resize::images::Image::{from_vec_u8, new, buffer}`, `Resizer::new()`, `ResizeOptions::new().resize_alg(...)`, `resize(&src,&mut dst,&opts)` for v6.0; `image::codecs::jpeg::JpegEncoder::new_with_quality(w, q).encode(&[u8], width, height, ExtendedColorType::Rgb8)` for image 0.25.

- [ ] **Step 4: Re-export + run**

In `ferrolite-catalog/src/lib.rs`, add `mod thumbnail;` and `pub use thumbnail::{generate_thumbnail, Thumbnail, ThumbnailStore, THUMB_MAX_EDGE, THUMB_QUALITY};`.

Run: `cargo test -p ferrolite-catalog thumbnail`
Expected: PASS (2 tests). Then `cargo clippy -p ferrolite-catalog --all-targets -- -D warnings` clean.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-catalog/src/thumbnail.rs ferrolite-catalog/src/lib.rs ferrolite-catalog/tests/catalog.rs
git commit -m "feat(catalog): ThumbnailStore trait + SQLite-BLOB impl + JPEG thumbnail generation"
```

---

### Task 8: `ferrolite-catalog` — folder ingest + full round-trip; workspace gate

**Files:**
- Create: `ferrolite-catalog/src/ingest.rs`
- Modify: `ferrolite-catalog/src/lib.rs` (add `mod ingest;`), `ferrolite-catalog/tests/catalog.rs` (add integration tests)

**Interfaces:**
- Consumes: `ferrolite_decode::{decode_preview, read_metadata}`; `Catalog` CRUD; `generate_thumbnail` + `ThumbnailStore`; `walkdir`; `rayon`.
- Produces: on `Catalog`, `ingest_folder(&self, path: &Path) -> Result<IngestSummary, CatalogError>` — walks RAWs, skips unchanged via `(mtime,size)`, decodes+thumbnails new/changed files in parallel, writes rows + thumbnail blobs serially, and updates `folders.last_scanned`.

- [ ] **Step 1: Write the failing integration tests**

Append to `ferrolite-catalog/tests/catalog.rs`:
```rust
use ferrolite_catalog::ThumbnailStore as _; // bring get_thumbnail into scope

/// Path to the shared fixture folder (contains the CC0 RAW from Task 2).
fn fixture_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/raw")
}

#[test]
fn ingest_folder_indexes_images_and_thumbnails() {
    let dir = tempdir();
    let cat = Catalog::open(&dir.join("catalog.db")).unwrap();

    let summary = cat.ingest_folder(&fixture_dir()).expect("ingest");
    assert!(summary.scanned >= 1, "should scan the fixture RAW");
    assert!(summary.added >= 1, "should add at least one image");
    assert_eq!(summary.failed, 0, "fixture must decode cleanly");
    assert!(cat.image_count().unwrap() >= 1);

    // Every indexed image has a decodable thumbnail within the size cap.
    let folder = cat.upsert_folder(&fixture_dir()).unwrap();
    let images = cat.list_images(folder).unwrap();
    assert!(!images.is_empty());
    for rec in images {
        let thumb = cat.get_thumbnail(rec.id).unwrap().expect("thumbnail present");
        assert!(thumb.width <= 256 && thumb.height <= 256);
        image::load_from_memory(&thumb.bytes).expect("thumb decodes");
    }
}

#[test]
fn second_ingest_skips_unchanged_files() {
    let dir = tempdir();
    let cat = Catalog::open(&dir.join("catalog.db")).unwrap();

    let first = cat.ingest_folder(&fixture_dir()).unwrap();
    assert!(first.added >= 1);

    let second = cat.ingest_folder(&fixture_dir()).unwrap();
    assert_eq!(second.added, 0, "nothing changed → no adds");
    assert_eq!(second.skipped, first.added, "all previously-added files skipped");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-catalog ingest`
Expected: FAIL (compile error — `ingest_folder` not defined).

- [ ] **Step 3: Write `ingest.rs`**

`ferrolite-catalog/src/ingest.rs`:
```rust
use crate::catalog::Catalog;
use crate::error::CatalogError;
use crate::model::{DecodeStatus, IngestSummary, NewImage};
use crate::thumbnail::{generate_thumbnail, Thumbnail, ThumbnailStore};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// RAW extensions we ingest (lowercased). Extend as camera coverage grows.
const RAW_EXTS: &[&str] = &[
    "nef", "nrw", "cr2", "cr3", "crw", "arw", "sr2", "srf", "raf", "rw2", "orf",
    "pef", "dng", "raw", "rwl", "iiq", "3fr", "erf", "mef", "mos", "kdc", "dcr",
];

fn is_raw(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| RAW_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// One file's CPU-heavy decode result, produced off the DB thread.
struct Decoded {
    filename: String,
    mtime: i64,
    size: i64,
    outcome: Result<(NewImage, Thumbnail), String>,
}

impl Catalog {
    /// Ingest a folder of RAWs (non-recursive into subfolders for this plan).
    /// New/changed files (by mtime+size) are decoded + thumbnailed in parallel;
    /// rows and thumbnail blobs are written serially (rusqlite Connection is
    /// single-threaded). Structured so Plan 3 can submit each file as a job.
    pub fn ingest_folder(&self, path: &Path) -> Result<IngestSummary, CatalogError> {
        let folder_id = self.upsert_folder(path)?;
        let mut summary = IngestSummary::default();

        // 1) Walk + stat (cheap, serial). Decide which files need (re)ingest.
        let mut to_process: Vec<(PathBuf, String, i64, i64)> = Vec::new();
        for entry in WalkDir::new(path).max_depth(1).into_iter().filter_map(|e| e.ok()) {
            let p = entry.path();
            if !p.is_file() || !is_raw(p) {
                continue;
            }
            summary.scanned += 1;
            let filename = entry.file_name().to_string_lossy().to_string();
            let meta = std::fs::metadata(p)?;
            let size = meta.len() as i64;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if self.needs_reingest(folder_id, &filename, mtime, size)? {
                to_process.push((p.to_path_buf(), filename, mtime, size));
            } else {
                summary.skipped += 1;
            }
        }

        // 2) Decode + thumbnail in parallel (no DB access here).
        let decoded: Vec<Decoded> = to_process
            .into_par_iter()
            .map(|(path, filename, mtime, size)| Decoded {
                filename,
                mtime,
                size,
                outcome: decode_one(&path, folder_id, mtime, size),
            })
            .collect();

        // 3) Write rows + thumbnails serially.
        for d in decoded {
            match d.outcome {
                Ok((new_image, thumb)) => {
                    let id = self.upsert_image(&new_image)?;
                    self.put_thumbnail(id, &thumb)?;
                    summary.added += 1;
                }
                Err(_msg) => {
                    // Record a failed row so the grid shows a placeholder and we
                    // don't retry forever. One bad file never downs the pass.
                    let failed = NewImage {
                        folder_id,
                        filename: d.filename,
                        mtime: d.mtime,
                        size: d.size,
                        make: None,
                        model: None,
                        width: None,
                        height: None,
                        orientation: ferrolite_image::Orientation::Normal,
                        capture_time: None,
                        iso: None,
                        decode_status: DecodeStatus::Failed,
                    };
                    self.upsert_image(&failed)?;
                    summary.failed += 1;
                }
            }
        }

        self.conn().execute(
            "UPDATE folders SET last_scanned = ?1 WHERE id = ?2",
            rusqlite::params![now_secs(), folder_id],
        )?;
        Ok(summary)
    }
}

/// Decode one file into a (row, thumbnail) pair. Returns Err(message) on any
/// decode/thumbnail failure so the caller can mark the row Failed.
fn decode_one(
    path: &Path,
    folder_id: i64,
    mtime: i64,
    size: i64,
) -> Result<(NewImage, Thumbnail), String> {
    let meta = ferrolite_decode::read_metadata(path).map_err(|e| e.to_string())?;
    let preview = ferrolite_decode::decode_preview(path).map_err(|e| e.to_string())?;
    let thumb = generate_thumbnail(&preview).map_err(|e| e.to_string())?;
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let new_image = NewImage {
        folder_id,
        filename,
        mtime,
        size,
        make: Some(meta.make),
        model: Some(meta.model),
        width: Some(meta.width),
        height: Some(meta.height),
        orientation: meta.orientation,
        capture_time: meta.capture_time,
        iso: meta.iso,
        decode_status: DecodeStatus::Done,
    };
    Ok((new_image, thumb))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```
Note: `read_metadata` and `decode_preview` each open the RAW independently. That double-open is acceptable for this plan (decode dominates either way); a single-open `decode_all` optimization is deferred to Plan 4 when the two-tier path needs it.

- [ ] **Step 4: Wire it in + run the integration tests**

In `ferrolite-catalog/src/lib.rs`, add `mod ingest;` (no new re-exports — `ingest_folder` is a method on the already-exported `Catalog`).

Run: `cargo test -p ferrolite-catalog`
Expected: PASS (migration 2 + CRUD 3 + thumbnail 2 + ingest 2 = 9). Then `cargo clippy -p ferrolite-catalog --all-targets -- -D warnings` clean.

- [ ] **Step 5: Full workspace gate**

Run:
```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```
Expected: fmt clean; clippy ZERO warnings across all four crates; all tests pass — existing app tests (module 2 + slider 6 + theme 3 + icon 5 + window_controls 3 = 19 from Plans 1 & chrome) **plus** ferrolite-image 10, ferrolite-decode 4, ferrolite-catalog 9. Fix any real clippy lints.

Note on CI: the existing `.github/workflows/ci.yml` runs `cargo clippy/build/test --all`, so the new crates are covered automatically. `rusqlite` with `bundled` compiles SQLite from source — the GitHub runners already have a C toolchain, and the Linux job's existing apt step is sufficient (no new system packages). No CI file change is required; confirm the 3-OS matrix stays green after pushing.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-catalog/src/ingest.rs ferrolite-catalog/src/lib.rs ferrolite-catalog/tests/catalog.rs
git commit -m "feat(catalog): folder ingest with incremental skip + parallel thumbnails — ingest/thumbnail/query round-trip"
```

---

## Self-Review

**Spec coverage (against `speed-core-design.md` + architecture map):**
- §3 crate decomposition (`image`/`decode`/`catalog`), tier purity (`ferrolite-image` zero-dep) → Tasks 1–8. ✓
- §4 schema (folders/images/thumbnails, indices, decode_status), thumbnails as SQLite BLOBs, `ThumbnailStore` trait for later swap → Tasks 5, 7. ✓
- §4 ingest flow (walk → upsert → `(mtime,size)` incremental skip → thumbnail jobs → indexed query) → Tasks 6, 8. ✓ (Synchronous + rayon-parallel here; job-system wrapping is Plan 3 per the plan sequence.)
- §4 catalog-is-a-cache invariant → `Catalog::open` rebuilds via re-ingest; documented; Global Constraints. ✓
- Contract §5.3 decode yields separable products `{preview, full, metadata}` → `decode_preview`/`decode_full`/`read_metadata` (Tasks 2–4). ✓
- §10 testing: pure-logic unit tests (orientation, pixel, DecodeStatus mapping), catalog integration tests (temp DB, fixture folder, incremental-skip, BLOB round-trip), decode tests (fixture preview/metadata/full). ✓ GPU golden-diffs + jobs/VT tests are correctly out of scope (Plans 3–4).
- §4 thumbnail format: design says WebP; deviated to JPEG with documented justification + reversible `format` column → Global Constraints. ✓
- **Deferred (correctly out of this plan):** `ferrolite-jobs`, Library grid UI + live status bar wiring, VT/viewer, two-tier crossfade, benchmark harness, metadata *writes*/XMP sidecars, the "fast partial full-decode" first-pixel fallback (needs demosaic → Plan 4). Mapped to Plans 3–5 / Spec 2. ✓

**Placeholder scan:** No TBD/TODO. The one genuinely unverified external API (rawler's `Rational` field names for aperture/shutter/focal) is isolated to the `rat` helper + three lines in Task 2 with an explicit "adjust to resolved rawler 0.7.2" note — the established convention from Plans 1 & chrome, not a logic placeholder. The fixture file is a download step (like Plan 1's fonts), with verification. `_msg` in Task 8 is an intentionally-unused bind (the error is collapsed to a Failed row); rename to `_` if clippy prefers.

**Type consistency:** `ImageBuffer`/`PixelFormat`/`Orientation` defined in Task 1, consumed in Tasks 3 (decode), 6–7 (catalog). `DecodeError` (Task 2) flows into `CatalogError::Decode` (Task 5) via `#[from]`. `Metadata` (Task 2) consumed by `decode_one` (Task 8). `NewImage`/`ImageRecord`/`DecodeStatus`/`IngestSummary` (Task 6) used by `upsert_image`/`ingest_folder` (Tasks 6, 8). `Thumbnail`/`ThumbnailStore`/`generate_thumbnail` (Task 7) used by `ingest_folder` (Task 8). `Catalog::conn()` (Task 5, `pub(crate)`) used by Tasks 6–8. `Catalog::upsert_folder`/`upsert_image`/`needs_reingest` signatures match their call sites in `ingest_folder`. Consistent. ✓

**Known execution risk:** rawler 0.7.2 surface (`RawSource`/`get_decoder`/`raw_image(dummy)`/`raw_metadata`/`Exif` fields/`RawImageData`) is verified against docs.rs but its API is not SemVer-stable — the `rust-build-resolver` agent is available if a patch differs. `image`-version unification with rawler (`cargo tree -i image`) is the other integration point; align our `image` requirement to rawler's if `DynamicImage` types mismatch.
