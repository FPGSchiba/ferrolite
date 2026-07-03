# Ingest Pipeline Optimization 2 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make JPEG-heavy ingest dramatically faster by DCT-scaled thumbnail decode (A), trim RAW bytes read via offset-directed reads (B), and self-tune read concurrency to the media (C).

**Architecture:** A adds a *new* size-targeted standard-decode function used only by the ingest/thumbnail path (viewer/export keep full-res). B parses the TIFF IFD in the RAW prefix to read the minimal preview+metadata span, falling back to today's tiered read. C wraps the ingest read in an adaptive gate driven by a ported Netflix/Vector gradient controller.

**Tech Stack:** Rust, `jpeg-decoder` 0.3, `image` 0.25, `rawler` 0.7, `fast_image_resize` 6, rayon, egui. Spec: [docs/superpowers/specs/2026-07-03-ingest-pipeline-2-design.md](../specs/2026-07-03-ingest-pipeline-2-design.md).

## Global Constraints

- **Never block the UI/update thread.** All decode/read/measurement runs on `ferrolite-jobs` / rayon ingest workers (CLAUDE.md rule 1).
- **Zero-overhead diagnostics when off.** Every added timing/counter for logging is gated behind `measure` / `diag::enabled()`; no `Instant`/allocation when off. (Exception: the C controller's own per-file latency timing is a functional feature and runs whenever adaptation is enabled — it is one `Instant` per file, not diagnostics.)
- **JPEG decoder:** `jpeg-decoder = "0.3"` (pure-Rust; already transitive).
- **Single source of truth for thumbnail size:** `ferrolite_catalog::THUMB_MAX_EDGE` (= 256). `ferrolite-decode` never hardcodes 256; the ingest layer passes it in as a `u32`.
- **`decode_preview_standard` MUST stay full-res** (viewer/load.rs:84 + export/batch.rs:105 depend on it). Only new functions downscale.
- **Rust gate:** `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`. On Windows, if `cargo test` deadlocks on a stale `target/debug` lock or LNK1104 (`ferrolite_app-<hash>.exe` locked), re-run with an isolated `CARGO_TARGET_DIR`.
- **After the gate is green, HOLD for the author's instrumented re-test** before finishing the branch. Git attribution disabled (no `Co-Authored-By`). Local merge to main; no push unless asked.
- Commit message types: `feat`/`fix`/`perf`/`refactor`/`docs`/`test`/`chore` (conventional commits).

---

# Phase A (PRIMARY) — downscale-decode standard-JPEG thumbnails

### Task A1: `decode_thumb_source_standard` — DCT-scaled JPEG decode with fallback

**Files:**
- Modify: `ferrolite-decode/Cargo.toml` (add `jpeg-decoder`)
- Modify: `ferrolite-decode/src/standard.rs` (add `StdThumbDecode` + `decode_thumb_source_standard`, keep `decode_preview_standard` unchanged)
- Test: inline `#[cfg(test)]` in `ferrolite-decode/src/standard.rs`

**Interfaces:**
- Consumes: `ferrolite_image::{ImageBuffer, Orientation, PixelFormat}`, `crate::orient::apply_orientation`, existing `read_exif`/`orientation_of` in standard.rs.
- Produces:
  ```rust
  pub struct StdThumbDecode {
      pub image: ImageBuffer,   // RGB8, EXIF-oriented
      pub dct_scale: u8,        // 1 | 2 | 4 | 8 (JPEG DCT factor); 1 for non-JPEG fallback
      pub decoded_w: u32,       // pre-orient, pre-resize decoded width
      pub decoded_h: u32,
  }
  pub fn decode_thumb_source_standard(
      path: &std::path::Path, target_edge: u32, measure: bool,
  ) -> Result<StdThumbDecode, DecodeError>;
  ```
  (`measure` is accepted for signature symmetry with the RAW path and future timing; A1 does not itself log.)

- [ ] **Step 1: Add the dependency**

In `ferrolite-decode/Cargo.toml`, under `[dependencies]`, add:
```toml
jpeg-decoder = "0.3"
```
Run `cargo tree -p ferrolite-decode -i jpeg-decoder` to confirm it resolves to 0.3.x.

- [ ] **Step 2: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `standard.rs`:
```rust
use super::{decode_thumb_source_standard, decode_preview_standard};
use ferrolite_image::PixelFormat;
use image::{ImageBuffer as ImgBuf, Rgb};

/// Encode a solid-ish RGB image to a temp .jpg and return its path.
fn temp_jpeg(name: &str, w: u32, h: u32) -> std::path::PathBuf {
    let mut img = ImgBuf::<Rgb<u8>, _>::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
    }
    let mut p = std::env::temp_dir();
    p.push(format!("ferrolite-std-test-{name}.jpg"));
    img.save(&p).unwrap();
    p
}

#[test]
fn thumb_decode_downscales_large_jpeg_via_dct() {
    let path = temp_jpeg("large", 4000, 3000);
    let out = decode_thumb_source_standard(&path, 256, false).unwrap();
    // 4000/256 -> smallest DCT factor keeping >=256 on an axis is 1/8 (=500x375).
    assert_eq!(out.dct_scale, 8, "expected 1/8 DCT scale for a 4000px source");
    assert!(out.decoded_w >= 256 || out.decoded_h >= 256);
    // decoded well under source: pixels reduced ~64x.
    assert!(out.decoded_w <= 600 && out.decoded_h <= 600);
    assert_eq!(out.image.format, PixelFormat::Rgb8);
    assert_eq!(out.image.width, out.decoded_w);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn thumb_decode_scale_one_matches_full_decode_dims() {
    // target larger than the image -> no DCT reduction (scale 1), same dims as full.
    let path = temp_jpeg("small", 200, 150);
    let thumb = decode_thumb_source_standard(&path, 256, false).unwrap();
    let full = decode_preview_standard(&path).unwrap();
    assert_eq!(thumb.dct_scale, 1);
    assert_eq!((thumb.image.width, thumb.image.height), (full.width, full.height));
    assert_eq!(thumb.image.pixels, full.pixels, "scale-1 pixels match full decode");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn thumb_decode_falls_back_for_png() {
    // PNG has no DCT path -> full decode + resize, scale reported as 1.
    let mut img = ImgBuf::<Rgb<u8>, _>::new(300, 200);
    for px in img.pixels_mut() { *px = Rgb([10, 20, 30]); }
    let mut p = std::env::temp_dir();
    p.push("ferrolite-std-test-fallback.png");
    img.save(&p).unwrap();
    let out = decode_thumb_source_standard(&p, 256, false).unwrap();
    assert_eq!(out.dct_scale, 1);
    assert_eq!((out.image.width, out.image.height), (300, 200));
    assert_eq!(out.image.format, PixelFormat::Rgb8);
    let _ = std::fs::remove_file(&p);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ferrolite-decode thumb_decode`
Expected: FAIL — `decode_thumb_source_standard` / `StdThumbDecode` not found.

- [ ] **Step 4: Implement**

Add to `standard.rs` (imports at top as needed: `use image::{DynamicImage, RgbImage};`, `use std::io::BufReader;`):
```rust
/// A downscaled preview for the *thumbnail* path only. JPEGs are DCT-scaled at
/// decode time (huge speedup vs a full 24 MP decode); other rasters fall back to
/// a full decode. NEVER used by the viewer/export full-res path.
#[derive(Debug, Clone)]
pub struct StdThumbDecode {
    pub image: ImageBuffer,
    pub dct_scale: u8,
    pub decoded_w: u32,
    pub decoded_h: u32,
}

/// True if `path` is a JPEG by extension AND magic bytes (`FF D8 FF`).
fn is_jpeg(path: &Path) -> bool {
    let ext_ok = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"))
        .unwrap_or(false);
    if !ext_ok {
        return false;
    }
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut magic = [0u8; 3];
    use std::io::Read;
    matches!(f.read_exact(&mut magic), Ok(())) && magic == [0xFF, 0xD8, 0xFF]
}

/// Decode a size-targeted preview for the thumbnail path. `target_edge` is the
/// thumbnail max edge (caller passes `THUMB_MAX_EDGE`); the JPEG path decodes at
/// the smallest DCT factor whose output stays >= `target_edge` on at least one
/// axis, so downstream resize has enough pixels but the IDCT does ~scale^2 less
/// work. EXIF orientation is applied (matching `decode_preview_standard`).
pub fn decode_thumb_source_standard(
    path: &Path,
    target_edge: u32,
    _measure: bool,
) -> Result<StdThumbDecode, DecodeError> {
    if is_jpeg(path) {
        decode_thumb_jpeg(path, target_edge)
    } else {
        // Non-JPEG: full decode + orient (same body as decode_preview_standard).
        let image = decode_preview_standard(path)?;
        let (w, h) = (image.width, image.height);
        Ok(StdThumbDecode { image, dct_scale: 1, decoded_w: w, decoded_h: h })
    }
}

fn decode_thumb_jpeg(path: &Path, target_edge: u32) -> Result<StdThumbDecode, DecodeError> {
    use jpeg_decoder::{Decoder, PixelFormat as JpegFmt};

    let file = std::fs::File::open(path)?;
    let mut dec = Decoder::new(BufReader::new(file));
    dec.read_info().map_err(|e| DecodeError::Rawler(format!("jpeg read_info: {e}")))?;
    let full = dec
        .info()
        .ok_or_else(|| DecodeError::Rawler("jpeg info missing".into()))?;
    let full_w = full.width as u32;

    // Ask for target on both axes; jpeg-decoder picks the smallest supported DCT
    // factor (1/8,1/4,1/2,1) yielding >= requested on at least one axis.
    let te = target_edge.min(u16::MAX as u32) as u16;
    let (sw, sh) = dec
        .scale(te, te)
        .map_err(|e| DecodeError::Rawler(format!("jpeg scale: {e}")))?;
    let pixels = dec
        .decode()
        .map_err(|e| DecodeError::Rawler(format!("jpeg decode: {e}")))?;
    let info = dec.info().ok_or_else(|| DecodeError::Rawler("jpeg info missing".into()))?;
    let (dw, dh) = (info.width as u32, info.height as u32);
    debug_assert_eq!((sw as u32, sh as u32), (dw, dh));

    // Normalize to RGB8. L8/RGB24 handled directly; L16/CMYK32 (rare) fall back
    // to a full image-crate decode for correctness.
    let rgb: Vec<u8> = match info.pixel_format {
        JpegFmt::RGB24 => pixels,
        JpegFmt::L8 => {
            let mut out = Vec::with_capacity(pixels.len() * 3);
            for g in pixels { out.extend_from_slice(&[g, g, g]); }
            out
        }
        _ => return decode_thumb_source_standard_fallback(path),
    };

    // Reuse the shared orientation logic via a DynamicImage.
    let orientation = read_exif(path).as_ref().map(orientation_of).unwrap_or(Orientation::Normal);
    let rgbimg = RgbImage::from_raw(dw, dh, rgb)
        .ok_or_else(|| DecodeError::Rawler("jpeg buffer length mismatch".into()))?;
    let oriented = crate::orient::apply_orientation(DynamicImage::ImageRgb8(rgbimg), orientation).to_rgb8();
    let (ow, oh) = (oriented.width(), oriented.height());
    let image = ImageBuffer::new(ow, oh, PixelFormat::Rgb8, oriented.into_raw())
        .expect("RGB8 buffer length is w*h*3 by construction");

    let dct_scale = (full_w / dw.max(1)).clamp(1, 8) as u8;
    Ok(StdThumbDecode { image, dct_scale, decoded_w: ow, decoded_h: oh })
}

/// Full-decode fallback wrapped as a StdThumbDecode (dct_scale = 1).
fn decode_thumb_source_standard_fallback(path: &Path) -> Result<StdThumbDecode, DecodeError> {
    let image = decode_preview_standard(path)?;
    let (w, h) = (image.width, image.height);
    Ok(StdThumbDecode { image, dct_scale: 1, decoded_w: w, decoded_h: h })
}
```
Export it: in `ferrolite-decode/src/lib.rs`, add `decode_thumb_source_standard, StdThumbDecode` to the `pub use standard::{...}` line.

> Note: `DecodeError` has no JPEG variant; the plan reuses `DecodeError::Rawler(String)` as a generic decode-error carrier. If the reviewer prefers, add a `DecodeError::Jpeg(String)` variant in `error.rs` instead — either is acceptable; pick one and keep it consistent.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ferrolite-decode` — all pass (the three new tests + existing).

- [ ] **Step 6: Commit**

```bash
git add ferrolite-decode/Cargo.toml ferrolite-decode/src/standard.rs ferrolite-decode/src/lib.rs Cargo.lock
git commit -m "feat(decode): DCT-scaled standard-JPEG thumbnail decode (decode_thumb_source_standard)"
```

---

### Task A2: route ingest single-pass through the downscaled decode + carry diag fields

**Files:**
- Modify: `ferrolite-decode/src/lib.rs` (`PreviewInfo` fields; `decode_meta_and_preview` Standard arm + `thumb_edge` param)
- Test: inline in `ferrolite-decode/src/lib.rs` (or exercised via existing decode tests)

**Interfaces:**
- Consumes: `decode_thumb_source_standard` (Task A1).
- Produces: extended `PreviewInfo` with `pub std_decode: Option<Duration>` and `pub dct_scale: Option<u8>`; new `decode_meta_and_preview` signature:
  ```rust
  pub fn decode_meta_and_preview(
      path: &Path, kind: FileKind, measure: bool, thumb_edge: u32,
  ) -> Result<(Metadata, ImageBuffer, PreviewInfo), DecodeError>;
  ```

- [ ] **Step 1: Extend `PreviewInfo`**

In `lib.rs`, add two fields to `PreviewInfo`:
```rust
    /// Standard (non-RAW) downscale-decode wall time (`Some` only when measured).
    pub std_decode: Option<Duration>,
    /// JPEG DCT scale factor used (1/2/4/8); `None` for RAW or non-JPEG fallback.
    pub dct_scale: Option<u8>,
```
Update the RAW arm's `PreviewInfo { .. }` literal to set `std_decode: None, dct_scale: None`.

- [ ] **Step 2: Update the Standard arm + signature**

Change the signature to add `thumb_edge: u32`. Replace the Standard arm body:
```rust
        FileKind::Standard => {
            let metadata = standard::read_metadata_standard(path)?;
            let t = measure.then(std::time::Instant::now);
            let dec = standard::decode_thumb_source_standard(path, thumb_edge, measure)?;
            let std_decode = t.map(|t| t.elapsed());
            let info = PreviewInfo {
                source: PreviewSource::EmbeddedPreview,
                src_w: dec.image.width,
                src_h: dec.image.height,
                source_kind: None,
                source_acquire: None,
                source_bytes: None,
                get_decoder: None,
                raw_metadata: None,
                raw_dims: None,
                extract: None,
                orient: None,
                std_decode,
                dct_scale: Some(dec.dct_scale),
            };
            Ok((metadata, dec.image, info))
        }
```

- [ ] **Step 3: Build to surface the caller break**

Run: `cargo build -p ferrolite-decode` — passes.
Run: `cargo build -p ferrolite-app` — FAILS at ingest.rs:526 (`decode_meta_and_preview` now needs `thumb_edge`). This is expected; Task A3 fixes the caller.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-decode/src/lib.rs
git commit -m "feat(decode): thread thumb_edge + std-decode/dct diag through decode_meta_and_preview"
```

---

### Task A3: wire ingest producer + regen path + slow-line diag

**Files:**
- Modify: `ferrolite-app/src/ingest.rs` (producer call ~526; regen ~725)
- Modify: `ferrolite-app/src/diag.rs` (`SlowSample` fields; `format_slow_line`; test data)
- Test: `ferrolite-app/src/diag.rs` inline tests

**Interfaces:**
- Consumes: `PreviewInfo.std_decode`, `PreviewInfo.dct_scale`, `StdThumbDecode` (via decode), `ferrolite_catalog::THUMB_MAX_EDGE`.
- Produces: `SlowSample` gains `pub std_decode_ms: f64` and `pub dct_scale: Option<u8>`.

- [ ] **Step 1: Extend `SlowSample` + failing formatter test**

In `diag.rs`, add to `SlowSample`:
```rust
    /// Standard-file decode wall time (ms); 0.0 for RAW.
    pub std_decode_ms: f64,
    /// JPEG DCT scale (1/2/4/8); None for RAW / non-JPEG.
    pub dct_scale: Option<u8>,
```
Include `std_decode_ms` in `measured_ms()` so `rest_ms()` no longer absorbs the standard decode:
```rust
    fn measured_ms(&self) -> f64 {
        self.source_acquire_ms + self.get_decoder_ms + self.raw_metadata_ms
            + self.raw_dims_ms + self.extract_ms + self.orient_ms + self.std_decode_ms
    }
```
Add a formatter test:
```rust
#[test]
fn format_slow_line_shows_std_decode_and_dct() {
    let mut s = sample_for_test(); // existing helper that builds a SlowSample
    s.is_raw = false;
    s.std_decode_ms = 48.0;
    s.dct_scale = Some(8);
    let out = format_slow_line(&s);
    assert!(out.contains("stddec 48"));
    assert!(out.contains("dct 1/8"));
}
```
(If no `sample_for_test` helper exists, construct the `SlowSample` inline mirroring the existing `format_slow_line_has_all_fields_ascii` test at diag.rs:~1600, adding the two new fields.)

- [ ] **Step 2: Update `format_slow_line`**

Add a `stddec` stage and a dct tag to the format string in `format_slow_line`. Insert `stddec {sd:.0}` into the stage group and append the dct tag after the tier:
```rust
    // add to the format string's stage list: ".../ orient {or:.0} / stddec {sd:.0} / rest {rest:.0}) ..."
    // and after "{kind} {w}x{h} {mp:.1}MP" add: " {dct}"
    sd = s.std_decode_ms,
    dct = match s.dct_scale { Some(n) => format!("dct 1/{n}"), None => String::from("dct -") },
```
Update the existing `format_slow_line_has_all_fields_ascii` test to set the two new fields (e.g. `std_decode_ms: 0.0, dct_scale: None`).

- [ ] **Step 3: Run formatter tests to verify pass**

Run: `cargo test -p ferrolite-app --lib diag::` — new + existing formatter tests pass.

- [ ] **Step 4: Fix the producer call + populate SlowSample**

In `ingest.rs` at ~526, pass the edge:
```rust
let decoded = ferrolite_decode::decode_meta_and_preview(
    &f.path, f.kind, measure, ferrolite_catalog::THUMB_MAX_EDGE,
);
```
In the `SlowSample { .. }` literal (~546), add:
```rust
    std_decode_ms: to_ms(info.std_decode),
    dct_scale: info.dct_scale,
```

- [ ] **Step 5: Downscale the regen path**

In `ingest.rs` at ~725, replace the `decode_preview` call so Standard files downscale (RAW keeps embedded-preview extraction):
```rust
let preview = match kind {
    FileKind::Standard => {
        ferrolite_decode::decode_thumb_source_standard(path, ferrolite_catalog::THUMB_MAX_EDGE, false)
            .map(|d| d.image)
            .map_err(|e| e.to_string())?
    }
    FileKind::Raw => ferrolite_decode::decode_preview(path, kind).map_err(|e| e.to_string())?,
};
```
(Ensure `use ferrolite_catalog::FileKind;` / correct path for `FileKind` is in scope in this fn.)

- [ ] **Step 6: Verify full workspace build + tests**

Run: `cargo build -p ferrolite-app` — passes.
Run: `cargo test -p ferrolite-app --lib` — passes.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/ingest.rs ferrolite-app/src/diag.rs
git commit -m "perf(app): route ingest thumbnails through DCT-scaled JPEG decode + attribute std-decode in slow diag"
```

- [ ] **Step 8: Phase-A gate**

Run: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`. All green before Phase B. (Windows lock workaround per Global Constraints if needed.)

---

# Phase B (secondary) — offset-directed RAW read

### Task B1: TIFF IFD preview-span parser

**Files:**
- Create: `ferrolite-decode/src/ifd.rs`
- Modify: `ferrolite-decode/src/lib.rs` (add `mod ifd;`)
- Test: inline `#[cfg(test)]` in `ifd.rs`

**Research step (do first, record decision in commit body):** Check whether `rawler` exposes a public cheap IFD walk over a byte slice (search `rawler`'s docs/source for `IFD`, `TiffReader`, `parse`). If a clean public API exists and avoids a new dependency, use it and skip the hand-rolled parser below (keep the same `PreviewSpan` return type + tests). Otherwise implement the minimal parser below (no new dependency — hand-rolled TIFF IFD0 walk is ~80 lines and fully covered by tests).

**Interfaces:**
- Produces:
  ```rust
  /// Minimal byte range that must be resident for rawler to extract the embedded
  /// preview + read metadata, parsed from a TIFF prefix. `end` is exclusive.
  pub struct PreviewSpan { pub end: u64 }
  /// Parse a TIFF/EXIF header at the start of `prefix`; return the max end offset
  /// (`offset + length`) among the embedded-preview strips (JPEGInterchangeFormat
  /// 513/514 and StripOffsets 273 / StripByteCounts 279) found in IFD0 + SubIFDs.
  /// Returns None if `prefix` is not a parseable TIFF or no preview pointer found.
  pub fn preview_span_end(prefix: &[u8]) -> Option<PreviewSpan>;
  ```

- [ ] **Step 1: Write failing tests**

Create `ifd.rs` with tests using a tiny synthetic little-endian TIFF:
```rust
#[cfg(test)]
mod tests {
    use super::preview_span_end;

    // Build a minimal LE TIFF: header + IFD0 with one tag JPEGInterchangeFormat
    // (513, LONG, value=offset) and JPEGInterchangeFormatLength (514, LONG, value=len).
    fn tiny_tiff(preview_off: u32, preview_len: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"II");            // little-endian
        b.extend_from_slice(&42u16.to_le_bytes()); // magic
        b.extend_from_slice(&8u32.to_le_bytes());   // IFD0 offset = 8
        // IFD0 at offset 8: entry count = 2
        b.extend_from_slice(&2u16.to_le_bytes());
        // entry: tag 513, type LONG(4), count 1, value
        let entry = |tag: u16, val: u32| {
            let mut e = Vec::new();
            e.extend_from_slice(&tag.to_le_bytes());
            e.extend_from_slice(&4u16.to_le_bytes()); // LONG
            e.extend_from_slice(&1u32.to_le_bytes()); // count
            e.extend_from_slice(&val.to_le_bytes());  // value/offset
            e
        };
        b.extend_from_slice(&entry(513, preview_off));
        b.extend_from_slice(&entry(514, preview_len));
        b.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0
        b
    }

    #[test]
    fn parses_jpeg_interchange_span() {
        let t = tiny_tiff(1000, 500);
        let span = preview_span_end(&t).expect("parsed");
        assert_eq!(span.end, 1500);
    }

    #[test]
    fn returns_none_on_garbage() {
        assert!(preview_span_end(&[0u8; 16]).is_none());
        assert!(preview_span_end(b"not a tiff").is_none());
    }

    #[test]
    fn handles_big_endian_header() {
        let mut b = Vec::new();
        b.extend_from_slice(b"MM");
        b.extend_from_slice(&42u16.to_be_bytes());
        b.extend_from_slice(&8u32.to_be_bytes());
        b.extend_from_slice(&1u16.to_be_bytes()); // 1 entry
        b.extend_from_slice(&513u16.to_be_bytes());
        b.extend_from_slice(&4u16.to_be_bytes());
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&2000u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        // No length tag -> span falls back to header-declared? Expect None (need both).
        assert!(preview_span_end(&b).is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-decode ifd::` — FAIL (module/functions absent).

- [ ] **Step 3: Implement the parser**

Implement `preview_span_end` in `ifd.rs`: detect `II`/`MM`, read u16/u32 with the detected endianness, bounds-check the IFD0 offset against `prefix.len()`, iterate entries (12 bytes each), collect `JPEGInterchangeFormat`(513)+`JPEGInterchangeFormatLength`(514) and any `StripOffsets`(273)+`StripByteCounts`(279) pairs, follow one level of `SubIFDs`(330) if present and in-bounds, and return `end = max(offset + length)`. Every read is bounds-checked; return `None` on any inconsistency. Add `mod ifd;` and `pub use ifd::{preview_span_end, PreviewSpan};` to `lib.rs`.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p ferrolite-decode ifd::` — PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-decode/src/ifd.rs ferrolite-decode/src/lib.rs
git commit -m "feat(decode): TIFF IFD parser for embedded-preview byte span (offset-directed RAW read)"
```

---

### Task B2: directed read in `with_ingest_source` + `Directed` tier diag

**Files:**
- Modify: `ferrolite-decode/src/source.rs` (`SourceKind` + directed read)
- Modify: `ferrolite-app/src/diag.rs` (`source_kind_label`, `record_source`, `[ingest-source]` line + `directed` counter)
- Test: inline in `source.rs` + `diag.rs`

**Interfaces:**
- Consumes: `crate::ifd::preview_span_end` (Task B1).
- Produces: `SourceKind::Directed` variant; `IngestProfile.directed` counter + accessor `directed()`.

- [ ] **Step 1: Add the `Directed` variant + failing source test**

In `source.rs`, add `Directed` to `SourceKind` (docstring: "offset-parsing found the exact preview span; read stopped there"). Add a test that a decode whose marker lies just past 1 MiB but whose *offset is parseable* is satisfied by a single directed read (use the existing `needs_byte` harness but prepend a `tiny_tiff`-style header pointing at the marker; assert `probe.kind == SourceKind::Directed` and `bytes` ≈ span end, not a full 8 MiB tier).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-decode source::` — FAIL (`Directed` unused / logic absent).

- [ ] **Step 3: Implement the directed read**

In `with_ingest_source`, after reading the initial 1 MiB prefix and before the tier loop: call `crate::ifd::preview_span_end(&buf)`. If `Some(span)` and `span.end` is within the file and `> buf.len()`, do one `read_up_to(&mut file, &mut buf, span.end as usize)`, run `f`; on success return `SourceKind::Directed`. On parse-miss, span-out-of-bounds, or `f` failure, fall through to the existing tier loop unchanged (the prefix bytes already read are reused — no re-read). Keep all existing behavior as the fallback path.

- [ ] **Step 4: Run source tests to verify pass**

Run: `cargo test -p ferrolite-decode source::` — PASS (new directed test + existing prefix/grown/full/eof/measure tests).

- [ ] **Step 5: Diag — count + label the Directed tier**

In `diag.rs`: add `directed: AtomicU64` to `IngestProfile`, handle `SourceKind::Directed` in `record_source` (bump `directed`), add `directed()` accessor, add `"directed"` arm to `source_kind_label`, and add a `directed {..}` field to the `[ingest-source]` line + its test. Update any exhaustive match on `SourceKind` the compiler flags.

- [ ] **Step 6: Verify + commit**

Run: `cargo test -p ferrolite-decode`; `cargo test -p ferrolite-app --lib diag::` — PASS.
```bash
git add ferrolite-decode/src/source.rs ferrolite-app/src/diag.rs
git commit -m "perf(decode): offset-directed RAW preview read with tiered fallback + Directed diag tier"
```

- [ ] **Step 7: Phase-B gate**

Run: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`. All green before Phase C.

---

# Phase C (secondary, novel) — adaptive read-concurrency controller

### Task C1: `ConcurrencyController` — pure gradient/AIMD logic

**Files:**
- Create: `ferrolite-app/src/read_gate.rs`
- Modify: `ferrolite-app/src/main.rs` (`mod read_gate;`) and `ferrolite-app/src/lib.rs` (`pub mod read_gate;`)
- Test: inline in `read_gate.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct ConcurrencyController { /* rtt_min_us, recent window, limit, bounds */ }
  impl ConcurrencyController {
      pub fn new(min_limit: usize, max_limit: usize, start: usize) -> Self;
      pub fn observe(&mut self, latency_us: u64);      // record one read latency
      pub fn recompute(&mut self) -> usize;            // gradient update -> new clamped limit
      pub fn limit(&self) -> usize;
      pub fn snapshot(&self) -> ControllerSnapshot;    // for diag
  }
  pub struct ControllerSnapshot { pub limit: usize, pub rtt_min_us: u64, pub rtt_recent_us: u64, pub gradient: f64 }
  ```

- [ ] **Step 1: Write failing tests (pure logic)**

```rust
#[cfg(test)]
mod tests {
    use super::ConcurrencyController;

    #[test]
    fn flat_fast_latency_grows_limit_toward_max() {
        let mut c = ConcurrencyController::new(1, 12, 4);
        for _ in 0..50 { c.observe(1000); c.recompute(); } // steady, at the floor
        assert!(c.limit() >= 8, "limit should climb when latency stays near rtt_min");
    }

    #[test]
    fn rising_latency_shrinks_limit() {
        let mut c = ConcurrencyController::new(1, 12, 10);
        c.observe(1000); c.recompute();          // establishes rtt_min ~1ms
        for _ in 0..20 { c.observe(8000); c.recompute(); } // 8x worse -> contention
        assert!(c.limit() < 10, "limit should shrink under rising latency");
    }

    #[test]
    fn limit_clamped_to_bounds() {
        let mut c = ConcurrencyController::new(2, 6, 6);
        for _ in 0..50 { c.observe(1_000_000); c.recompute(); }
        assert!(c.limit() >= 2, "never below min");
        let mut c2 = ConcurrencyController::new(2, 6, 2);
        for _ in 0..50 { c2.observe(500); c2.recompute(); }
        assert!(c2.limit() <= 6, "never above max");
    }
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p ferrolite-app --lib read_gate::tests::flat_fast` — FAIL (type absent).

- [ ] **Step 3: Implement the controller**

Implement the gradient update: keep `rtt_min_us` = min observed, a short rolling mean `rtt_recent_us` (e.g. EWMA or fixed-window of last N), `gradient = rtt_min / rtt_recent` clamped to `(0,1]`. On `recompute`: `new = limit * gradient + queue_allowance` (e.g. allowance = `sqrt(limit)` rounded, or a constant 1.0), round, clamp to `[min_limit, max_limit]`. Guard against zero/absent samples (return current limit). Snapshot returns the current fields.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p ferrolite-app --lib read_gate::` — PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/read_gate.rs ferrolite-app/src/main.rs ferrolite-app/src/lib.rs
git commit -m "feat(app): pure gradient concurrency controller for adaptive ingest reads"
```

---

### Task C2: `AdaptiveReadGate` — resizable permit gate + env override

**Files:**
- Modify: `ferrolite-app/src/read_gate.rs` (add the sync wrapper)
- Test: inline in `read_gate.rs`

**Interfaces:**
- Consumes: `ConcurrencyController` (Task C1).
- Produces:
  ```rust
  pub struct AdaptiveReadGate { /* Mutex<State{controller,in_flight}> + Condvar */ }
  pub struct ReadPermit<'a> { /* holds &gate + start Instant; Drop records latency + decrements + notifies */ }
  impl AdaptiveReadGate {
      /// max_limit = worker count. Reads FERROLITE_INGEST_READ_CONCURRENCY: if set
      /// to N>=1, pins the limit (adaptation disabled); 0/unset => adaptive.
      pub fn new(max_limit: usize) -> Self;
      pub fn acquire(&self) -> ReadPermit<'_>;         // blocks while in_flight >= limit
      pub fn snapshot(&self) -> super::ControllerSnapshot;
      pub fn is_pinned(&self) -> bool;
  }
  ```

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn gate_blocks_beyond_limit_then_releases() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let gate = Arc::new(AdaptiveReadGate::with_pinned_limit(2)); // test ctor pinning limit=2
    let peak = Arc::new(AtomicUsize::new(0));
    let cur = Arc::new(AtomicUsize::new(0));
    let mut hs = vec![];
    for _ in 0..8 {
        let (g, p, c) = (gate.clone(), peak.clone(), cur.clone());
        hs.push(std::thread::spawn(move || {
            let _permit = g.acquire();
            let now = c.fetch_add(1, Ordering::SeqCst) + 1;
            p.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            c.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for h in hs { h.join().unwrap(); }
    assert!(peak.load(Ordering::SeqCst) <= 2, "never more than 2 concurrent permits");
}

#[test]
fn env_override_pins_limit() {
    // with_pinned_limit models the FERROLITE_INGEST_READ_CONCURRENCY=N path.
    let gate = AdaptiveReadGate::with_pinned_limit(3);
    assert!(gate.is_pinned());
    assert_eq!(gate.snapshot().limit, 3);
}
```
(Provide a test-only `with_pinned_limit(n)` ctor so tests don't touch process env; the real `new` reads the env var and calls it or the adaptive path.)

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p ferrolite-app --lib read_gate::tests::gate_blocks` — FAIL.

- [ ] **Step 3: Implement the gate**

`State { controller: ConcurrencyController, in_flight: usize }` behind `Mutex`, plus a `Condvar`. `acquire`: lock, wait on condvar while `in_flight >= controller.limit()`, then `in_flight += 1`, record start `Instant`, return `ReadPermit`. `ReadPermit::drop`: compute elapsed µs, lock, `controller.observe(us)`, periodically (every release) `controller.recompute()`, `in_flight -= 1`, `notify_all` (limit may have grown). `new(max)`: read `FERROLITE_INGEST_READ_CONCURRENCY`; `Ok(n)` with `n>=1` => pinned controller (`min=max_limit_field=n`, adaptation no-op); else adaptive `ConcurrencyController::new(1, max, start=(max/2).max(2))`. Add `with_pinned_limit` (test + internal reuse) and `is_pinned`.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p ferrolite-app --lib read_gate::` — PASS. (These tests spawn threads; they are timing-tolerant by using `fetch_max` on observed concurrency, not sleeps-as-assertions.)

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/read_gate.rs
git commit -m "feat(app): resizable adaptive read-permit gate with env override"
```

---

### Task C3: wire the gate into ingest + `[ingest-concurrency]` diag

**Files:**
- Modify: `ferrolite-app/src/ingest.rs` (create gate; acquire per file in the `for_each_with` closure)
- Modify: `ferrolite-app/src/diag.rs` (`[ingest-concurrency]` line + F9 counters; `IngestSummary`/overlay fields)
- Test: `diag.rs` formatter test for the new line

**Interfaces:**
- Consumes: `AdaptiveReadGate` (Task C2), `ControllerSnapshot`.

- [ ] **Step 1: Failing formatter test for the concurrency line**

In `diag.rs` add a `format_ingest_concurrency(snap: &ConcurrencySnapshot) -> String` (a small diag struct mirroring `ControllerSnapshot` + `inflight_peak`) and a test:
```rust
#[test]
fn format_ingest_concurrency_line() {
    let out = format_ingest_concurrency(&ConcurrencySnapshot {
        limit: 5, rtt_min_us: 1200, rtt_recent_us: 3400, gradient: 0.35, inflight_peak: 6, pinned: false,
    });
    assert!(out.starts_with("[ingest-concurrency]"));
    assert!(out.contains("limit 5"));
    assert!(out.contains("gradient 0.35"));
}
```

- [ ] **Step 2: Run to verify fail, then implement the formatter**

Run: `cargo test -p ferrolite-app --lib diag::tests::format_ingest_concurrency_line` — FAIL, then implement `ConcurrencySnapshot` + `format_ingest_concurrency` and rerun to PASS.

- [ ] **Step 3: Create + wire the gate in ingest**

In `ingest.rs`, before `to_process.par_iter()`: build `let read_gate = std::sync::Arc::new(crate::read_gate::AdaptiveReadGate::new(rayon::current_num_threads().max(1)));`. Inside the `for_each_with` closure, acquire around the decode:
```rust
let _permit = read_gate.acquire();
let decoded = ferrolite_decode::decode_meta_and_preview(
    &f.path, f.kind, measure, ferrolite_catalog::THUMB_MAX_EDGE,
);
drop(_permit); // release before CPU-heavy resize/encode below
```
(Clone the `Arc` into the closure via `for_each_with`'s state tuple or a `move` capture as the existing `sender` pattern allows.)

- [ ] **Step 4: Emit the concurrency summary (gated)**

After the parallel pass (near the existing `[ingest-source]` emit, still under `if let Some(p) = profile`), take `read_gate.snapshot()` + inflight peak and emit `format_ingest_concurrency(..)` via the diag log sink. Optionally surface `limit`/`gradient` in the F9 overlay `IngestSummary` fields (follow the existing `by kind` line pattern). All emission gated behind `diag::enabled()`; the gate itself runs regardless (functional).

- [ ] **Step 5: Verify build + tests**

Run: `cargo build -p ferrolite-app`; `cargo test -p ferrolite-app --lib` — PASS.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/ingest.rs ferrolite-app/src/diag.rs
git commit -m "perf(app): adaptive read-concurrency gate on ingest + [ingest-concurrency] diag"
```

- [ ] **Step 7: Full gate**

Run: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`. All green.

---

## Post-implementation: HOLD for author's instrumented re-test

Do **not** finish/merge the branch after the gate is green. Per CLAUDE.md, hand off to the author (Jann) for an instrumented run (`FERROLITE_DIAG=1`) on the real JPEG-only library:
1. Baseline (checkout of merge-base or the un-optimized numbers already recorded).
2. Confirm **A**: `[ingest-summary]` `by kind ... std ... (decode p50 ...)` drops sharply; standard `[ingest-slow]` lines show `stddec` small and `dct 1/8`; total wall-clock falls.
3. Confirm **B**: `[ingest-source]` shows a `directed` share; `source_bytes` totals drop vs baseline.
4. Confirm **C**: `[ingest-concurrency]` converges; A/B `FERROLITE_INGEST_READ_CONCURRENCY=2|3|4|6` vs adaptive to decide the shipped default.
Address any issues the author finds, then use superpowers:finishing-a-development-branch.

## Self-Review

- **Spec coverage:** A (Tasks A1–A3), B (B1–B2), C (C1–C3), diag extensions, and the HOLD gate all map to spec sections. ✓
- **`decode_preview_standard` untouched** (viewer/export full-res preserved); only new fns downscale. ✓
- **THUMB_MAX_EDGE single source of truth** (passed as `u32` from ingest; decode never hardcodes 256). ✓
- **Zero-overhead diag** preserved; the C gate's own timing is called out as functional (not diag). ✓
- **Type consistency:** `StdThumbDecode`, `PreviewInfo.{std_decode,dct_scale}`, `SlowSample.{std_decode_ms,dct_scale}`, `SourceKind::Directed`, `ConcurrencyController`/`AdaptiveReadGate`/`ControllerSnapshot`/`ConcurrencySnapshot` names used consistently across tasks. ✓
- **Open implementation choice flagged, not hidden:** B1 research (rawler IFD vs hand-rolled); `DecodeError::Rawler` vs new `Jpeg` variant in A1. Both have a stated default. ✓
