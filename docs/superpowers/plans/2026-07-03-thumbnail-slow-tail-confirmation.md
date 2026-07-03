# Thumbnail Slow-Tail Confirmation Instrumentation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add conclusive, zero-overhead-off instrumentation that identifies *why* a right-skewed tail of RAW files takes 5–6 s each to decode during ingest, plus fix the mojibake in the existing ingest summary — without changing decode behavior.

**Architecture:** `ferrolite-decode` gains a pure-data `PreviewInfo` (which embedded-image branch won + embedded dims + optional extract/orient sub-timings) returned from `decode_meta_and_preview`; it stays diagnostics-agnostic and only times when the app passes `measure=true`. `ferrolite-app`'s `diag` module records slow-file samples (gated on `FERROLITE_DIAG`) and emits a per-file line + an end-of-ingest aggregate. No fix to decode strategy this round — that is a later spec.

**Tech Stack:** Rust, `rawler` 0.7.2, `image`, egui app; existing `FERROLITE_DIAG` dev-mode in `ferrolite-app/src/diag.rs`.

## Global Constraints

- Never block the UI/update thread; all decode work already runs on `ferrolite-jobs` workers — this plan only reads timings taken there. (CLAUDE.md)
- Instrumentation MUST be zero-overhead when `FERROLITE_DIAG` is unset: `measure=false` performs **no** `Instant` calls; no slow-sample recording; no aggregate emission. (CLAUDE.md + spec §E)
- `ferrolite-decode` MUST NOT depend on the diag module or read `FERROLITE_DIAG`; it only returns data and times when asked via `measure: bool`. (spec §A)
- Rust style: `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings` clean, `Result`/`?`, no `unwrap()` outside tests. (rules/rust)
- No git attribution trailers. (project)
- Final gate: `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green, then HOLD for the author's hands-on instrumented run before finishing the branch. (CLAUDE.md + spec verification gate)

## File Structure

- `ferrolite-app/src/diag.rs` — MODIFY: ASCII fix in `format_ingest_summary`; add `SlowSample`, `record_slow`/`slow_samples`, `slow_threshold_ms()`, `format_slow_line`, `format_slow_aggregate`, `emit_slow_aggregate`, `source_label`; unit tests.
- `ferrolite-decode/src/preview.rs` — MODIFY: add `PreviewSource`, `PreviewInfo`; `preview_from_decoder` gains `measure: bool` and returns `(ImageBuffer, PreviewInfo)`; update `decode_preview_raw`.
- `ferrolite-decode/src/lib.rs` — MODIFY: re-export `PreviewSource`/`PreviewInfo`; `decode_meta_and_preview` gains `measure: bool` and returns `(Metadata, ImageBuffer, PreviewInfo)`.
- `ferrolite-decode/tests/decode.rs` — MODIFY: update `combined_matches_separate_paths` to the new signature; add a `PreviewInfo` test.
- `ferrolite-app/src/ingest.rs` — MODIFY: pass `measure = diag::enabled()`, destructure the 3-tuple, record slow samples + per-file line, emit aggregate after the summary.

---

### Task 1: ASCII-only ingest summary (fix mojibake)

Independent, tiny, no dependencies. `format_ingest_summary` currently emits `Σ` (`\u{03a3}`) and `→` (`\u{2192}`), which render as `Î£`/`â†'` in the Windows console.

**Files:**
- Modify: `ferrolite-app/src/diag.rs` (`format_ingest_summary`, ~lines 832-839; test `format_ingest_summary_contains_all_sections`, ~lines 1204-1214)

**Interfaces:**
- Consumes: nothing new.
- Produces: `format_ingest_summary(&IngestSummary) -> String` now contains only ASCII bytes (behavior-compatible field values, only the `Σ`/`→` glyphs change to `sum`/`->`).

- [ ] **Step 1: Write the failing test.** Add to the `#[cfg(test)] mod tests` in `diag.rs`:

```rust
#[test]
fn format_ingest_summary_is_ascii_only() {
    let s = IngestSummary {
        files: 3320,
        wall_s: 251.1,
        decode_sum_s: 2438.0,
        decode_par_s: 250.9,
        cores: 10,
        ..Default::default()
    };
    let out = format_ingest_summary(&s);
    assert!(
        out.is_ascii(),
        "ingest summary must be ASCII (no mojibake on Windows); got: {out}"
    );
}
```

- [ ] **Step 2: Run it to confirm it fails.**

Run: `cargo test -p ferrolite-app --lib diag::tests::format_ingest_summary_is_ascii_only`
Expected: FAIL (assertion fails — output currently contains `Σ`/`→`).

- [ ] **Step 3: Replace the glyphs.** In `format_ingest_summary`, change the format string lines that contain `\u{03a3}` and `\u{2192}`:

Replace:
```rust
         \x20decode  \u{03a3} {dsum:.0}s / {cores} cores \u{2192} {sp:.1}x | p50 {p50:.0}ms p95 {p95:.0}ms max {mx:.0}ms\n\
         \x20encode  \u{03a3} {esum:.1}s  avg {eavg:.0}ms\n\
         \x20upsert  {ub} batches  avg {uavg:.0}ms (\u{03a3} {usum:.1}s)\n\
```
With:
```rust
         \x20decode  sum {dsum:.0}s / {cores} cores -> {sp:.1}x | p50 {p50:.0}ms p95 {p95:.0}ms max {mx:.0}ms\n\
         \x20encode  sum {esum:.1}s  avg {eavg:.0}ms\n\
         \x20upsert  {ub} batches  avg {uavg:.0}ms (sum {usum:.1}s)\n\
```

- [ ] **Step 4: Fix the existing assertion.** The current test `format_ingest_summary_contains_all_sections` asserts `out.contains("6.8x")` — that still holds (the `->` is separate). No `Σ`/`→` substring is asserted, so no change is needed there. Verify by reading the test; if any assertion references `Σ`/`→`, update it to `sum`/`->`.

- [ ] **Step 5: Run tests to confirm they pass.**

Run: `cargo test -p ferrolite-app --lib diag::tests`
Expected: PASS (both the new ASCII test and `format_ingest_summary_contains_all_sections`).

- [ ] **Step 6: Commit.**

```bash
git add ferrolite-app/src/diag.rs
git commit -m "diag(app): ASCII-only ingest summary (fix sum/-> mojibake on Windows)"
```

---

### Task 2: `ferrolite-decode` — `PreviewInfo` + `measure` sub-timing

Adds the decode-side reporting. This changes `decode_meta_and_preview`'s signature, so the single app call site (`ingest.rs:525`) and the decode test are updated in the same task to keep the workspace compiling.

**Files:**
- Modify: `ferrolite-decode/src/preview.rs`
- Modify: `ferrolite-decode/src/lib.rs` (`decode_meta_and_preview` ~lines 49-75; add re-export near line 20-21)
- Modify: `ferrolite-decode/tests/decode.rs` (`combined_matches_separate_paths` ~lines 38-50)
- Modify: `ferrolite-app/src/ingest.rs:525` (compile-fix only: pass `false`, bind `_info`)

**Interfaces:**
- Consumes: `rawler::decoders::Decoder` (`preview_image`/`full_image`/`thumbnail_image`), `apply_orientation` (preview.rs), `ImageBuffer` (`ferrolite_image`).
- Produces:
  - `pub enum PreviewSource { EmbeddedPreview, FullImage, EmbeddedThumbnail }` (Copy, PartialEq, Eq, Debug)
  - `pub struct PreviewInfo { pub source: PreviewSource, pub src_w: u32, pub src_h: u32, pub extract: Option<std::time::Duration>, pub orient: Option<std::time::Duration> }` (Copy, Debug)
  - `pub fn decode_meta_and_preview(path: &Path, kind: FileKind, measure: bool) -> Result<(Metadata, ImageBuffer, PreviewInfo), DecodeError>`
  - `pub(crate) fn preview_from_decoder(decoder: &dyn Decoder, src: &RawSource, exif_orientation: u16, measure: bool) -> Result<(ImageBuffer, PreviewInfo), DecodeError>`

- [ ] **Step 1: Add the types + rewrite `preview_from_decoder` in `preview.rs`.** Replace the current `preview_from_decoder` (lines 11-29) and add the types at the top of the file (after the imports). Add `use std::time::{Duration, Instant};` to the imports.

```rust
/// Which embedded image the RAW preview path used. In rawler 0.7.2 no decoder
/// implements `preview_image`, so RAW previews come from `full_image` (the
/// full-resolution embedded JPEG) or, rarely, `thumbnail_image`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSource {
    EmbeddedPreview,
    FullImage,
    EmbeddedThumbnail,
}

/// What the preview extraction did, for diagnostics. `source`/`src_w`/`src_h`
/// are always populated (free); `extract`/`orient` are `Some` only when the
/// caller passes `measure = true` (zero `Instant` cost when false).
#[derive(Debug, Clone, Copy)]
pub struct PreviewInfo {
    pub source: PreviewSource,
    pub src_w: u32,
    pub src_h: u32,
    pub extract: Option<Duration>,
    pub orient: Option<Duration>,
}

/// Extract an upright RGB8 preview using an already-constructed decoder and the
/// EXIF orientation already read from its metadata. Shared by `decode_preview_raw`
/// and the single-pass `decode_meta_and_preview` so the file is parsed once.
/// When `measure` is true, times the embedded-image decode (`extract`) separately
/// from the orientation + RGB8 conversion (`orient`).
pub(crate) fn preview_from_decoder(
    decoder: &dyn Decoder,
    src: &RawSource,
    exif_orientation: u16,
    measure: bool,
) -> Result<(ImageBuffer, PreviewInfo), DecodeError> {
    let params = RawDecodeParams::default();

    let t_extract = measure.then(Instant::now);
    let (dynimg, source) = if let Some(img) = decoder.preview_image(src, &params).ok().flatten() {
        (img, PreviewSource::EmbeddedPreview)
    } else if let Some(img) = decoder.full_image(src, &params).ok().flatten() {
        (img, PreviewSource::FullImage)
    } else if let Some(img) = decoder.thumbnail_image(src, &params).ok().flatten() {
        (img, PreviewSource::EmbeddedThumbnail)
    } else {
        return Err(DecodeError::NoPreview(std::path::PathBuf::new()));
    };
    let extract = t_extract.map(|t| t.elapsed());
    let (src_w, src_h) = (dynimg.width(), dynimg.height());

    let t_orient = measure.then(Instant::now);
    let oriented = apply_orientation(dynimg, Orientation::from_exif(exif_orientation));
    let rgb = oriented.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    let buf = ImageBuffer::new(w, h, PixelFormat::Rgb8, rgb.into_raw())
        .expect("RGB8 buffer length is w*h*3 by construction");
    let orient = t_orient.map(|t| t.elapsed());

    Ok((
        buf,
        PreviewInfo {
            source,
            src_w,
            src_h,
            extract,
            orient,
        },
    ))
}
```

- [ ] **Step 2: Update `decode_preview_raw` in `preview.rs`.** It ignores the new `PreviewInfo` and passes `measure = false`. Replace its call:

```rust
        preview_from_decoder(decoder.as_ref(), src, exif_orientation, false)
            .map(|(buf, _info)| buf)
            .map_err(|_| DecodeError::NoPreview(path.to_path_buf()))
```

- [ ] **Step 3: Update `lib.rs`.** Add the re-export next to the existing `pub use` lines (after line 21):

```rust
pub use preview::{PreviewInfo, PreviewSource};
```

Then change `decode_meta_and_preview` (lines 49-75) to thread `measure` and return `PreviewInfo`:

```rust
pub fn decode_meta_and_preview(
    path: &Path,
    kind: FileKind,
    measure: bool,
) -> Result<(Metadata, ImageBuffer, PreviewInfo), DecodeError> {
    match kind {
        FileKind::Raw => crate::source::with_ingest_source(path, |src| {
            let decoder = rawler::get_decoder(src).map_err(rawler_err)?;
            let params = RawDecodeParams::default();

            let meta_raw = decoder.raw_metadata(src, &params).map_err(rawler_err)?;
            // `dummy = true`: geometry only, no pixel decode (fast on an in-memory source).
            let dims = decoder.raw_image(src, &params, true).map_err(rawler_err)?;
            let exif_orientation = meta_raw.exif.orientation.unwrap_or(1);

            let metadata = build_metadata_from_raw(&meta_raw, &dims)?;
            let (preview, info) =
                crate::preview::preview_from_decoder(decoder.as_ref(), src, exif_orientation, measure)
                    .map_err(|_| DecodeError::NoPreview(path.to_path_buf()))?;
            Ok((metadata, preview, info))
        }),
        FileKind::Standard => {
            let metadata = standard::read_metadata_standard(path)?;
            let preview = standard::decode_preview_standard(path)?;
            // Standard rasters are read directly (not a RAW fallback branch) and
            // are already fast; tag as EmbeddedPreview and leave sub-timings None.
            let info = PreviewInfo {
                source: PreviewSource::EmbeddedPreview,
                src_w: preview.width,
                src_h: preview.height,
                extract: None,
                orient: None,
            };
            Ok((metadata, preview, info))
        }
    }
}
```

- [ ] **Step 4: Update the decode test.** In `ferrolite-decode/tests/decode.rs`, change `combined_matches_separate_paths` to destructure the 3-tuple and pass `false`:

```rust
#[test]
fn combined_matches_separate_paths() {
    let (m, p, _info) =
        ferrolite_decode::decode_meta_and_preview(&fixture(), FileKind::Raw, false).expect("combined");
    let m2 = ferrolite_decode::read_metadata(&fixture(), FileKind::Raw).expect("metadata");
    let p2 = ferrolite_decode::decode_preview(&fixture(), FileKind::Raw).expect("preview");
    assert_eq!(m, m2, "combined metadata should match separate read_metadata");
    assert_eq!((p.width, p.height), (p2.width, p2.height));
    assert_eq!(p.pixels, p2.pixels, "preview pixels should be identical");
}
```

- [ ] **Step 5: Add the `PreviewInfo` test.** Append to `ferrolite-decode/tests/decode.rs`:

```rust
#[test]
fn preview_info_reports_dims_and_gated_timings() {
    use ferrolite_decode::PreviewSource;
    // measure = true: dims populated, sub-timings present, source is a RAW branch.
    let (_m, p, info) =
        ferrolite_decode::decode_meta_and_preview(&fixture(), FileKind::Raw, true).expect("measured");
    assert!(info.src_w > 0 && info.src_h > 0, "embedded dims should be > 0");
    assert_eq!((info.src_w, info.src_h), (p.width, p.height).max_by_dims(),
        "src dims are the embedded (pre-resize) dims; here no resize, so they match the buffer up to orientation");
    assert!(info.extract.is_some() && info.orient.is_some(), "measured => Some timings");
    assert!(matches!(
        info.source,
        PreviewSource::FullImage | PreviewSource::EmbeddedThumbnail | PreviewSource::EmbeddedPreview
    ));

    // measure = false: no timings recorded.
    let (_m2, _p2, info2) =
        ferrolite_decode::decode_meta_and_preview(&fixture(), FileKind::Raw, false).expect("unmeasured");
    assert!(info2.extract.is_none() && info2.orient.is_none(), "unmeasured => None timings");
}
```

Note: `preview_from_decoder` applies orientation, which may transpose dims, so `info.src_w/src_h` (pre-orient) can differ from `p.width/height` (post-orient) for rotated shots. Simplify the dims assertion to avoid a false failure — replace the second `assert_eq!` above with:

```rust
    assert!(
        (info.src_w == p.width && info.src_h == p.height)
            || (info.src_w == p.height && info.src_h == p.width),
        "embedded dims match the buffer up to a 90-degree orientation swap"
    );
```

(Delete the `.max_by_dims()` placeholder line entirely — it is not a real method.)

- [ ] **Step 6: Compile-fix the app call site.** In `ferrolite-app/src/ingest.rs`, change line 525 and its match arm so the workspace compiles (the real slow-recording wiring is Task 4). Change:

```rust
                let decoded = ferrolite_decode::decode_meta_and_preview(&f.path, f.kind);
```
to:
```rust
                let decoded = ferrolite_decode::decode_meta_and_preview(&f.path, f.kind, false);
```
and change the Ok arm pattern (line 530) from `Ok((meta, preview)) => {` to `Ok((meta, preview, _info)) => {`.

- [ ] **Step 7: Run tests.**

Run: `cargo test -p ferrolite-decode` then `cargo build --workspace`
Expected: decode tests PASS; workspace builds clean.

- [ ] **Step 8: Commit.**

```bash
git add ferrolite-decode/src/preview.rs ferrolite-decode/src/lib.rs ferrolite-decode/tests/decode.rs ferrolite-app/src/ingest.rs
git commit -m "decode: PreviewInfo (branch + embedded dims + gated extract/orient timing)"
```

---

### Task 3: `diag` slow-sample storage, aggregation, and formatters (pure)

All pure additions to `diag.rs` with unit tests; no ingest wiring yet.

**Files:**
- Modify: `ferrolite-app/src/diag.rs`

**Interfaces:**
- Consumes: `ferrolite_decode::PreviewSource`; existing `percentile(&[u32], f64) -> u32`, `write_log`, `enabled`, `IngestProfile`.
- Produces:
  - `pub struct SlowSample { pub decode_ms: f64, pub extract_ms: f64, pub orient_ms: f64, pub is_raw: bool, pub src_w: u32, pub src_h: u32, pub source: ferrolite_decode::PreviewSource, pub model: String, pub path: String }` (Clone, Debug)
  - `pub fn source_label(s: ferrolite_decode::PreviewSource) -> &'static str`
  - `pub fn slow_threshold_ms() -> f64` (env `FERROLITE_DIAG_SLOW_MS`, default 500.0, cached)
  - `pub fn format_slow_line(s: &SlowSample) -> String`
  - `pub fn format_slow_aggregate(samples: &[SlowSample], total_files: usize) -> String`
  - `pub fn emit_slow_aggregate(samples: &[SlowSample], total_files: usize)` (gated)
  - `IngestProfile::record_slow(&self, s: SlowSample)` and `IngestProfile::slow_samples(&self) -> Vec<SlowSample>`
  - const `SLOW_TOP_N: usize = 10`

- [ ] **Step 1: Write failing tests.** Add to `#[cfg(test)] mod tests` in `diag.rs`:

```rust
fn slow_sample(decode_ms: f64, ex: f64, or_: f64, w: u32, h: u32, src: ferrolite_decode::PreviewSource, model: &str, path: &str) -> SlowSample {
    SlowSample {
        decode_ms,
        extract_ms: ex,
        orient_ms: or_,
        is_raw: true,
        src_w: w,
        src_h: h,
        source: src,
        model: model.to_string(),
        path: path.to_string(),
    }
}

#[test]
fn format_slow_line_has_all_fields_ascii() {
    use ferrolite_decode::PreviewSource;
    let s = slow_sample(5305.0, 5100.0, 190.0, 6048, 4032, PreviewSource::FullImage, "ILCE-7M4", "C:/x/DSC1234.ARW");
    let out = format_slow_line(&s);
    assert!(out.starts_with("[ingest-slow]"));
    assert!(out.contains("5305ms"));
    assert!(out.contains("extract 5100"));
    assert!(out.contains("orient 190"));
    assert!(out.contains("rest 15")); // 5305 - 5100 - 190
    assert!(out.contains("6048x4032"));
    assert!(out.contains("24.4MP"));
    assert!(out.contains("via full_image"));
    assert!(out.contains("ILCE-7M4"));
    assert!(out.is_ascii());
}

#[test]
fn format_slow_aggregate_reports_counts_sources_models_and_top() {
    use ferrolite_decode::PreviewSource;
    let samples = vec![
        slow_sample(6000.0, 5800.0, 150.0, 6048, 4032, PreviewSource::FullImage, "ILCE-7M4", "a.ARW"),
        slow_sample(5000.0, 4800.0, 150.0, 6048, 4032, PreviewSource::FullImage, "ILCE-7M4", "b.ARW"),
        slow_sample(700.0,  120.0,  520.0, 8256, 5504, PreviewSource::EmbeddedThumbnail, "NIKON Z 7", "c.NEF"),
    ];
    let out = format_slow_aggregate(&samples, 3320);
    assert!(out.contains("[ingest-slow-summary] 3 slow files"));
    assert!(out.contains("full_image 2"));
    assert!(out.contains("thumbnail 1"));
    assert!(out.contains("ILCE-7M4"));
    assert!(out.contains("NIKON Z 7"));
    // top-slowest section lists the 6000ms file first.
    let top_idx = out.find("top ").expect("has a top section");
    assert!(out[top_idx..].contains("6000ms"));
    assert!(out.is_ascii());
}

#[test]
fn slow_threshold_defaults_to_500_when_unset() {
    // Note: reads the process env once (cached). In CI the var is unset.
    assert_eq!(slow_threshold_ms(), 500.0);
}

#[test]
fn ingest_profile_records_slow_samples() {
    use ferrolite_decode::PreviewSource;
    let p = IngestProfile::default();
    p.record_slow(slow_sample(5305.0, 5100.0, 190.0, 6048, 4032, PreviewSource::FullImage, "m", "p"));
    assert_eq!(p.slow_samples().len(), 1);
}
```

- [ ] **Step 2: Run tests to confirm they fail.**

Run: `cargo test -p ferrolite-app --lib diag::tests`
Expected: FAIL to compile (symbols not defined).

- [ ] **Step 3: Add the import + `SlowSample` + `source_label` + threshold.** Near the top of `diag.rs` (after existing `use` lines), add the const and (below the `IngestSummary`/`percentile` region) the types/functions:

```rust
/// Max slowest files listed in the aggregate block.
const SLOW_TOP_N: usize = 10;

/// One slow-decode sample, recorded only when profiling is on and a file's total
/// decode time crossed `slow_threshold_ms()`.
#[derive(Debug, Clone)]
pub struct SlowSample {
    pub decode_ms: f64,
    pub extract_ms: f64,
    pub orient_ms: f64,
    pub is_raw: bool,
    pub src_w: u32,
    pub src_h: u32,
    pub source: ferrolite_decode::PreviewSource,
    pub model: String,
    pub path: String,
}

impl SlowSample {
    fn megapixels(&self) -> f64 {
        (self.src_w as f64 * self.src_h as f64) / 1_000_000.0
    }
}

/// Short ASCII label for a preview source (used in logs).
pub fn source_label(s: ferrolite_decode::PreviewSource) -> &'static str {
    use ferrolite_decode::PreviewSource::*;
    match s {
        EmbeddedPreview => "preview",
        FullImage => "full_image",
        EmbeddedThumbnail => "thumbnail",
    }
}

/// Slow-file logging threshold in ms: `FERROLITE_DIAG_SLOW_MS` if set and valid,
/// else 500. Cached once.
pub fn slow_threshold_ms() -> f64 {
    static T: OnceLock<f64> = OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("FERROLITE_DIAG_SLOW_MS")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| *v >= 0.0)
            .unwrap_or(500.0)
    })
}
```

- [ ] **Step 4: Add the formatters.** Add below `emit_ingest_summary`:

```rust
/// One-line slow-file record. ASCII-only.
pub fn format_slow_line(s: &SlowSample) -> String {
    let rest = (s.decode_ms - s.extract_ms - s.orient_ms).max(0.0);
    format!(
        "[ingest-slow] {dec:.0}ms (extract {ex:.0} / orient {or:.0} / rest {rest:.0}) \
         {kind} {w}x{h} {mp:.1}MP via {src} model={model:?} {path:?}",
        dec = s.decode_ms,
        ex = s.extract_ms,
        or = s.orient_ms,
        rest = rest,
        kind = if s.is_raw { "RAW" } else { "std" },
        w = s.src_w,
        h = s.src_h,
        mp = s.megapixels(),
        src = source_label(s.source),
        model = s.model,
        path = s.path,
    )
}

/// End-of-ingest aggregate over all slow samples. ASCII-only.
pub fn format_slow_aggregate(samples: &[SlowSample], total_files: usize) -> String {
    use ferrolite_decode::PreviewSource;
    if samples.is_empty() {
        return format!("[ingest-slow-summary] 0 slow files (of {total_files})");
    }
    let n = samples.len();
    let share = if total_files > 0 {
        100.0 * n as f64 / total_files as f64
    } else {
        0.0
    };
    let extract_sum_s: f64 = samples.iter().map(|s| s.extract_ms).sum::<f64>() / 1000.0;
    let orient_sum_s: f64 = samples.iter().map(|s| s.orient_ms).sum::<f64>() / 1000.0;

    let count_src = |src: PreviewSource| samples.iter().filter(|s| s.source == src).count();
    let by_source = format!(
        "full_image {f} | thumbnail {t} | preview {p}",
        f = count_src(PreviewSource::FullImage),
        t = count_src(PreviewSource::EmbeddedThumbnail),
        p = count_src(PreviewSource::EmbeddedPreview),
    );

    // by model: stable order by descending count, then name.
    let mut models: Vec<&str> = samples.iter().map(|s| s.model.as_str()).collect();
    models.sort_unstable();
    models.dedup();
    let mut model_rows: Vec<(usize, String)> = models
        .iter()
        .map(|m| {
            let ms: Vec<u32> = samples
                .iter()
                .filter(|s| s.model == *m)
                .map(|s| s.decode_ms as u32)
                .collect();
            (ms.len(), format!("{m:?} {} (p50 {}ms)", ms.len(), percentile(&ms, 0.5)))
        })
        .collect();
    model_rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let by_model = model_rows
        .iter()
        .map(|(_, r)| r.as_str())
        .collect::<Vec<_>>()
        .join(" | ");

    // top-N slowest by decode_ms desc, tie-broken by path for determinism.
    let mut top: Vec<&SlowSample> = samples.iter().collect();
    top.sort_by(|a, b| {
        b.decode_ms
            .partial_cmp(&a.decode_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    let top_lines = top
        .iter()
        .take(SLOW_TOP_N)
        .map(|s| {
            format!(
                "  {dec:.0}ms {w}x{h} via {src} {path:?}",
                dec = s.decode_ms,
                w = s.src_w,
                h = s.src_h,
                src = source_label(s.source),
                path = s.path,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "[ingest-slow-summary] {n} slow files ({share:.1}% of {total_files})  \
         extract-sum {es:.1}s  orient-sum {os:.1}s\n\
         \x20by source  {by_source}\n\
         \x20by model   {by_model}\n\
         \x20top {topn} slowest:\n{top_lines}",
        n = n,
        share = share,
        total_files = total_files,
        es = extract_sum_s,
        os = orient_sum_s,
        by_source = by_source,
        by_model = by_model,
        topn = top.len().min(SLOW_TOP_N),
        top_lines = top_lines,
    )
}

/// Emit the slow aggregate to the diag sink. No-op when diag is off.
pub fn emit_slow_aggregate(samples: &[SlowSample], total_files: usize) {
    if !enabled() {
        return;
    }
    write_log(&format_slow_aggregate(samples, total_files));
}
```

- [ ] **Step 5: Add `IngestProfile` slow storage.** In the `IngestProfile` struct (fields near lines 705-719) add a field:

```rust
    slow: Mutex<Vec<SlowSample>>,
```

and in the `impl IngestProfile` block add:

```rust
    pub fn record_slow(&self, s: SlowSample) {
        if let Ok(mut v) = self.slow.lock() {
            v.push(s);
        }
    }
    pub fn slow_samples(&self) -> Vec<SlowSample> {
        self.slow.lock().map(|v| v.clone()).unwrap_or_default()
    }
```

(`IngestProfile` derives `Default`; `Mutex<Vec<_>>` is `Default`, so no change to the derive.)

- [ ] **Step 6: Run tests to confirm they pass.**

Run: `cargo test -p ferrolite-app --lib diag::tests`
Expected: PASS (all new tests + existing).

- [ ] **Step 7: Commit.**

```bash
git add ferrolite-app/src/diag.rs
git commit -m "diag(app): slow-file sample store, aggregate + per-file formatters"
```

---

### Task 4: Wire slow logging into `ingest_job`

Connect the pieces: measure when diag is on, record + log slow files, emit the aggregate after the summary.

**Files:**
- Modify: `ferrolite-app/src/ingest.rs` (decode call ~525; Ok arm ~530-568; summary block ~611-650)

**Interfaces:**
- Consumes: `decode_meta_and_preview(.., measure)` + `PreviewInfo` (Task 2); `SlowSample`, `slow_threshold_ms`, `format_slow_line`, `emit_slow_aggregate`, `IngestProfile::record_slow`/`slow_samples` (Task 3).
- Produces: no new public API; runtime behavior only (gated logs).

- [ ] **Step 1: Capture decode time + pass `measure`.** Replace the decode block (lines 524-528) with:

```rust
                let t_meta = profile.as_ref().map(|_| std::time::Instant::now());
                let measure = crate::diag::enabled();
                let decoded = ferrolite_decode::decode_meta_and_preview(&f.path, f.kind, measure);
                let decode_us = t_meta.map(|t| t.elapsed().as_micros() as u64);
                if let (Some(us), Some(p)) = (decode_us, profile.as_ref()) {
                    p.record_decode(us, is_raw);
                }
```

- [ ] **Step 2: Record + log slow files in the Ok arm.** The Ok arm pattern is `Ok((meta, preview, _info))` from Task 2 — rename `_info` to `info`. Immediately inside the arm (before `NewImage::from_metadata`), add:

```rust
                        if let (Some(p), Some(us)) = (profile.as_ref(), decode_us) {
                            let decode_ms = us as f64 / 1000.0;
                            if decode_ms >= crate::diag::slow_threshold_ms() {
                                let to_ms = |d: Option<std::time::Duration>| {
                                    d.map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
                                };
                                let sample = crate::diag::SlowSample {
                                    decode_ms,
                                    extract_ms: to_ms(info.extract),
                                    orient_ms: to_ms(info.orient),
                                    is_raw,
                                    src_w: info.src_w,
                                    src_h: info.src_h,
                                    source: info.source,
                                    model: meta.model.clone(),
                                    path: f.path.display().to_string(),
                                };
                                crate::diag::write_log(&crate::diag::format_slow_line(&sample));
                                p.record_slow(sample);
                            }
                        }
```

(`meta` is used by borrow at `NewImage::from_metadata(.., &meta, ..)` further down, so `meta.model.clone()` here is fine.)

- [ ] **Step 3: Emit the aggregate after the summary.** In the summary block, right after `crate::diag::emit_ingest_summary(&summary);` (line 649), add:

```rust
        crate::diag::emit_slow_aggregate(&p.slow_samples(), file_count);
```

(`p` is the `&Arc<IngestProfile>` bound in `if let (Some(p), Some(t)) = (&profile, t_job)`; `file_count` is the total already used at `files: file_count`.)

- [ ] **Step 4: Build + clippy + workspace tests.**

Run:
```bash
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: builds clean, no clippy warnings, all tests PASS.

- [ ] **Step 5: Commit.**

```bash
git add ferrolite-app/src/ingest.rs
git commit -m "diag(app): record + log slow-decode files and emit ingest-slow aggregate"
```

---

### Task 5: Final gate + hold for author

**Files:** none (verification only).

- [ ] **Step 1: Full gate.**

Run:
```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all green. If `cargo test` hits `LNK1104: cannot open ...ferrolite_app-<hash>.exe` (Windows target-dir lock), re-run with an isolated target dir instead of killing the process:
```bash
CARGO_TARGET_DIR=target/diag-gate cargo test --workspace
```

- [ ] **Step 2: HOLD.** Do NOT merge/PR/finish. Present the branch to the author (Jann) for a hands-on instrumented run:

```
FERROLITE_DIAG=1 <run the app>   # optional: FERROLITE_DIAG_SLOW_MS=500
```
Ask the author to trigger a full ingest and share the new `[ingest-slow]` per-file lines + `[ingest-slow-summary]` block. Compare against the 251 s / p95 5.3 s baseline. Only after the author's feedback do we design round 2 (the fix).

---

## Self-Review

**Spec coverage:**
- Spec §A (PreviewSource/PreviewInfo, `measure`, sub-timing, diag-agnostic, Standard tagging) → Task 2. ✔
- Spec §B (per-file slow line, aggregate, threshold env, `write_log` sink, IngestProfile storage) → Tasks 3 + 4. ✔
- Spec §C (ASCII fix + test assertions) → Task 1. ✔
- Spec §D (pure unit tests for aggregation/formatters/MP + ASCII assertion; fixture-gated decode test) → Tasks 1, 2 (decode), 3 (diag). ✔
- Spec §E (non-goals: no decode-strategy change; zero-overhead-off) → enforced by Global Constraints + Task 4 `measure = enabled()`; no fix task present. ✔
- Verification gate (fmt/clippy/test then hold) → Task 5. ✔

**Placeholder scan:** No TBD/TODO. The one placeholder-looking token (`.max_by_dims()`) is explicitly called out as not-real and removed in Task 2 Step 5. ✔

**Type consistency:** `decode_meta_and_preview(.., measure: bool) -> (Metadata, ImageBuffer, PreviewInfo)` (Task 2) is consumed with a 3-tuple destructure in Task 2 Step 6 and Task 4 Step 1. `PreviewInfo { source, src_w, src_h, extract: Option<Duration>, orient: Option<Duration> }` fields match their reads in `format_slow_line`/Task 4. `SlowSample` fields defined in Task 3 match construction in Task 4. `source_label`, `slow_threshold_ms`, `format_slow_line`, `format_slow_aggregate`, `emit_slow_aggregate`, `record_slow`, `slow_samples` names are consistent across Tasks 3 and 4. ✔

**MP/s note:** The spec mentioned MP/s as a format proxy; the author then chose explicit extract/orient sub-timing. The per-file line therefore reports MP and the extract/orient/rest split (sub-timing supersedes the proxy); MP/s is derivable from `MP` and `extract` if needed and is intentionally omitted to keep the line readable. ✔
