# Thumbnail-generation slow-tail — confirmation instrumentation (design)

**Date:** 2026-07-03
**Branch:** `fix/thumbnail-decode-tail`
**Status:** design, awaiting author review

## Problem

A full ingest of the author's ~3320-image RAW library takes **~251 s**. The merged
`FERROLITE_DIAG` `[ingest-summary]` already pins *where* the time goes:

```
phases  scan 0.0s  phaseA 0.1s  filter 0.0s  decode(par) 250.9s
decode  sum 2438s / 10 cores -> 9.7x | p50 123ms p95 5305ms max 6540ms
encode  sum 12.1s  avg 4ms
upsert  64 batches  avg 11ms (sum 0.7s)
channel max depth 1 | producer done@251.1s consumer done@251.1s (tail 0.0s)
by kind  RAW 3307 (decode p50 123ms) | std 13
```

Ruled out (with evidence): serial phases (0.1 s), rayon parallelism (9.7x of 10
cores — near perfect), thumbnail encode (12.1 s, 4 ms avg), SQLite upsert (0.7 s),
consumer/DB tail (0.0 s). The bottleneck is **`decode_meta_and_preview` CPU on a
heavy right-skewed tail**: p50 123 ms, p95 5.3 s, max 6.5 s, mean ~6x the median.

## Corrected mechanism (from reading `rawler` 0.7.2)

The originally-stated hypothesis was "slow files have no embedded preview, so rawler
falls back to a full sensor demosaic (seconds)." Reading the code shows a different —
and more actionable — mechanism:

The RAW preview path is `preview_from_decoder`
(`ferrolite-decode/src/preview.rs`):

```rust
decoder.preview_image(...)               // tried 1st
  .or_else(|| decoder.full_image(...))       // tried 2nd
  .or_else(|| decoder.thumbnail_image(...))  // tried 3rd
```

Two facts from `rawler` 0.7.2 source:

1. **No decoder implements `preview_image`.** Only the default trait method exists
   (`decoders/mod.rs`), returning `Ok(None)` with a warning. So step 1 is a
   guaranteed miss for every RAW and we always fall to `full_image`.
2. **None of these three methods demosaic.** In 0.7.2 they each extract an
   *embedded JPEG/TIFF* (`arw.rs`, `cr3.rs`, `nef.rs`, `dng.rs` all read a
   `JPEGInterchangeFormat`/`mdat`/sub-IFD blob and `image::load_from_memory`).
   `full_image` extracts the **full-resolution** embedded preview (commonly
   ~24 MP); `thumbnail_image` extracts a small (~160 px) one.

So the real cost is: **we always decode the full-resolution embedded image
(~24 MP JPEG), apply its orientation transform, and `to_rgb8` it, then resize down
to a 256 px thumbnail** (`THUMB_MAX_EDGE = 256`). The p50 (123 ms) is a normal-size
embedded-JPEG decode; the 5–6 s tail is the files whose embedded full image is
largest or slowest to decode (huge JPEGs, or previews stored uncompressed / as
lossless-JPEG TIFF via `dynamic_image_from_ifd`, plus a 24 MP orientation-transpose
for rotated shots).

This reframes the eventual fix from "avoid a demosaic" to **"stop decoding a 24 MP
image when we only need 256 px."** The confirmation instrumentation below is
designed to measure exactly the quantities that discriminate the candidate fixes.

## Scope of this spec (confirm first)

Per the author's direction, this branch proceeds in two design rounds:

- **This spec / round 1 (built now):** the cosmetic ASCII fix + conclusive
  slow-file instrumentation. Deliverable: the author runs one instrumented ingest
  (`FERROLITE_DIAG=1`) and we read the new slow-file data.
- **Round 2 (spec'd AFTER the run):** the concrete fix, chosen against the data
  using the decision criteria at the end of this document. It gets its own
  spec + plan iteration in the same branch.

No fix is implemented this round.

## Design

### A. `ferrolite-decode` — report which embedded image was used + sub-timings

`ferrolite-decode` stays diagnostics-agnostic (no `FERROLITE_DIAG` dependency). It
gains the ability to *report* what it did and *optionally* time its two sub-steps.

New pure-data types (in `ferrolite-decode`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSource {
    EmbeddedPreview,   // decoder.preview_image()  (never happens in 0.7.2, kept for correctness)
    FullImage,         // decoder.full_image()
    EmbeddedThumbnail, // decoder.thumbnail_image()
}

#[derive(Debug, Clone, Copy)]
pub struct PreviewInfo {
    pub source: PreviewSource,      // always set: which or_else branch won
    pub src_w: u32,                 // always set: embedded image width  (pre-resize)
    pub src_h: u32,                 // always set: embedded image height (pre-resize)
    pub extract: Option<Duration>,  // Some iff measure=true: embedded-decode call time
    pub orient:  Option<Duration>,  // Some iff measure=true: apply_orientation + to_rgb8 time
}
```

Signature change (single caller — `ingest_job`):

```rust
pub fn decode_meta_and_preview(
    path: &Path, kind: FileKind, measure: bool,
) -> Result<(Metadata, ImageBuffer, PreviewInfo), DecodeError>
```

`preview_from_decoder` gains a `measure: bool` and returns `(ImageBuffer,
PreviewInfo)`:

- `source` is set from whichever `or_else` branch produced the image.
- `src_w`/`src_h` come from the decoded image before orientation/resize (free).
- When `measure=true`, wrap the branch resolution (`preview/full/thumbnail_image`
  incl. `image::load_from_memory`) in one `Instant` → `extract`, and wrap
  `apply_orientation(...).to_rgb8()` in another → `orient`.
- When `measure=false`, **no `Instant` calls occur** — literally zero overhead off,
  honoring the CLAUDE.md "instrumentation zero-overhead when flag is off" rule.

The `FileKind::Standard` arm returns `PreviewInfo` with
`source = EmbeddedPreview`-equivalent semantics (it is a standard raster, not a RAW
fallback); it may set `src_w/src_h` from the decoded preview and leave `extract`/
`orient` `None` (standard files are already fast — `std` p50 is negligible — and are
not the investigation target). Concretely we tag standard files with a dedicated
value; see the ambiguity note below.

> Ambiguity resolution: `PreviewSource` describes the RAW fallback branch. For
> `FileKind::Standard` we set `source = PreviewSource::EmbeddedPreview` (the closest
> "a real preview was read directly" meaning) and populate dims; standard files are
> excluded from the slow-tail aggregate anyway because they are not RAW. This keeps
> the enum small and avoids a `Standard` variant that would leak file-kind into a
> decode-source concept.

### B. `ferrolite-app` — per-file slow log + end-of-ingest aggregate (gated)

All `FERROLITE_DIAG` gating lives here.

In `ingest_job` (`ferrolite-app/src/ingest.rs`, around the existing
`decode_meta_and_preview` call + `record_decode`):

- Pass `measure = diag::enabled()` into `decode_meta_and_preview`.
- After `record_decode`, when `diag::enabled()` **and** the total `decode_ms >=
  slow_threshold_ms`, record a slow-file sample into `IngestProfile`.

Threshold: `FERROLITE_DIAG_SLOW_MS` env var, default **500 ms** (p50 is 123 ms, so
500 ms captures the tail without flooding the log). Parsed once and cached like
`mode()`.

Each slow sample carries:

- `decode_ms` (total, already measured),
- `extract_ms` / `orient_ms` (from `PreviewInfo`; residual = total − extract − orient
  attributes metadata/dummy-dims/setup),
- `kind`, `src_w × src_h`, derived `MP` and `MP/s`,
- `PreviewSource`,
- camera `model` (from `Metadata`),
- file path.

Two outputs, both through the existing `diag::write_log` sink (stderr + session
file):

1. **Per-file line** emitted as each slow file finishes:
   `[ingest-slow] 5305ms (extract 5100 / orient 190 / rest 15) RAW 6048x4032 24.4MP via full_image model="ILCE-7M4" "…/DSC1234.ARW"`
2. **Aggregate block** appended after `[ingest-summary]`:
   - slow-file count and share of total,
   - breakdown by `PreviewSource`,
   - breakdown by camera `model` (count + p50 decode_ms),
   - split of total slow time into `extract` vs `orient` (which sub-step dominates),
   - top-N slowest files (N configurable constant, default 10).

The slow samples live in `IngestProfile` behind a `Mutex<Vec<SlowSample>>` (same
pattern as the existing `raw_us`/`std_us` sample vectors), pushed only when
profiling is on — a brief push per slow file, negligible against a >500 ms decode.

### C. Cosmetic ASCII fix

`format_ingest_summary` (`ferrolite-app/src/diag.rs`) currently emits `\u{03a3}`
(Σ) and `\u{2192}` (→), which render as mojibake (`Î£` / `â†'`) in the Windows
console. Replace: `Σ` → `sum`, `→` → `->`. Update the two affected assertions in the
existing `format_ingest_summary_contains_all_sections` test (`6.8x` speedup line and
the `sum`-prefixed lines).

### D. Testing

Unit tests, pure (no fixtures), matching the existing `diag.rs` test style:

- slow-sample aggregation: counts, share, by-source, by-model (count + p50),
  extract-vs-orient split, top-N ordering (ties broken deterministically),
- the `[ingest-slow]` per-file line formatter (fields present, ms rounding),
- `MP` / `MP/s` derivation (incl. guard against 0-area / 0-time),
- an assertion that `format_ingest_summary` output is ASCII-only (no non-ASCII
  bytes), locking the mojibake fix.

`ferrolite-decode`: a light, fixture-gated test (reuse the existing
`../fixtures/raw/sample.rw2` skip-if-absent pattern) asserting `decode_meta_and_preview(.., measure=true)`
returns a `PreviewInfo` with non-zero dims and `Some` sub-timings, and that
`measure=false` yields `None` sub-timings.

### E. Non-goals / constraints

- No change to which embedded image is chosen, no resize/decode strategy change —
  that is round 2.
- Never block the UI/update thread: all decode work stays on `ferrolite-jobs`
  (unchanged); the instrumentation only reads timings already taken on the worker.
- Zero overhead when `FERROLITE_DIAG` is unset: `measure=false` (no `Instant`s), no
  slow-sample recording, no aggregate emission.

## Candidate fixes (round 2 — documented, NOT built here)

- **Fix A — prefer smallest adequate embedded image:** try `thumbnail_image` first,
  use it if its short edge ≥ 256 px, else `full_image`. Stays within rawler's public
  API; only helps files whose embedded thumbnail is already ≥ 256 px.
- **Fix B — DCT-downscaled decode:** decode the embedded JPEG at 1/2–1/8 DCT scale
  (via `jpeg-decoder` scaling), so extract + orient + `to_rgb8` all run on a small
  buffer. Largest expected win; more invasive (needs the embedded bytes, bypassing
  rawler's eager full decode); no help for non-JPEG embedded previews.
- **Fix C — defer the slow tail:** index the catalog row immediately, generate the
  slow thumbnail lazily / at low priority so the grid fills fast and slow ones
  trickle in. Architectural; complements A/B and interacts with the two-tier cache
  and lazy-load system.

### Decision criteria (evaluated against the instrumented run)

- Slow files are **large-MP + high MP/s via `full_image`, `extract`-dominated** →
  **Fix B** (downscaled decode) is the direct win.
- Slow files are **`orient`-dominated** → the transpose/`to_rgb8` on a 24 MP buffer
  is the cost → any fix that yields a *small* buffer early (B) fixes it; a
  standalone orientation optimization is a fallback.
- Slow files show **low MP/s at modest dims** (uncompressed / lossless-JPEG TIFF) →
  B (DCT scaling) will not help those; consider a format-specific path or Fix C.
- A subset already has **≥ 256 px embedded thumbnails** → **Fix A** as a cheap
  partial win, possibly combined with B.
- Slow set is small and irreducibly CPU-bound → **Fix C** to hide latency.

## Verification gate

Automated gate (necessary, not sufficient): `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
Then **hold** for the author's hands-on instrumented run and comparison of the new
slow-file data against the 251 s / p95 5.3 s baseline before designing round 2.
