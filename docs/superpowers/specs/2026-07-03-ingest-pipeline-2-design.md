# Ingest Pipeline Optimization 2 — Design

**Date:** 2026-07-03
**Branch:** `perf/ingest-pipeline-2`
**Status:** approved (brainstorming), pending implementation plan

## Context

Ingest (import a card/folder → grid fills with thumbnails) is the user's first
entry point; making it fast improves the whole app. Prior sessions took a
~3320-image library on a slow SD card from 252 s → 122 s → 50 s. That work was
entirely on the **RAW** path: `with_ingest_source` no longer mmap-reads whole
24–50 MB RAW files to slice a ≤2 MB embedded preview; it does an incremental
single-open sequential read (1 MiB → 2/4/8 MiB → EOF) reporting a
`SourceKind {Prefix, Grown, Full}` + bytes read. Decode/encode/DB are negligible;
remaining RAW cost is per-file SD open+read latency, near the hardware floor.

**Standard rasters take a different path** and were never optimized.
`decode_preview_standard` (standard.rs) fully decodes a JPEG at native resolution
(e.g. 6016×4016 ≈ 24 MP, ~0.5–0.6 s) and only then resizes to a 256 px thumbnail.
For a large **JPEG-only** library that full-res decode *is* the ingest cost — the
slowest instrumented files were exactly `converted/*.jpg` at ~500–628 ms each.

This spec covers three improvements. **A is primary** (the JPEG-only win); B and C
are secondary but in scope for this branch. Order: **A → B → C**.

### Load-bearing constraint (confirmed in code)

`decode_preview_standard` is shared by three callers:
- ingest thumbnails (`decode_meta_and_preview` Standard arm at lib.rs:135; regen at
  ingest.rs:725),
- the **viewer** (viewer/load.rs:59), and
- **export** (export/batch.rs:105).

viewer/load.rs:84-87 explicitly documents that for a Standard image the tier-1
preview from `decode_preview_standard` **is** the full-resolution image and tier-2
is skipped. Therefore **the downscale must only apply to the ingest/thumbnail
path**; the viewer and export must keep receiving full-res JPEGs. This is enforced
by adding a *new dedicated function* rather than changing `decode_preview_standard`.

### Research findings (JPEG DCT-scaled decode)

Verified against current docs (2026-07):

| Crate | DCT-scaled decode? | Notes |
|---|---|---|
| `image` 0.25 `JpegDecoder` | No | Backend is zune-jpeg 0.5; `new()` + trait methods only |
| `zune-jpeg` 0.5 | No | `DecoderOptions` = colorspace/limits only |
| **`jpeg-decoder` 0.3.2** | **Yes** | `scale(w,h)` picks smallest DCT factor (1/8,1/4,1/2,1) ≥ requested on ≥1 axis; call before `decode()`. Pure-Rust; was `image`'s JPEG backend pre-0.25 |

Decision: **`jpeg-decoder` 0.3** for JPEG; `image::open` fallback for other standards.

Adaptive-concurrency (Improvement C): Netflix/Vector **gradient** algorithm
(`gradient = rtt_min / rtt_recent`, additive-increase / multiplicative-decrease) is
the proven approach. All Rust crates (`rate_limiter_aimd`, `tower-resilience-adaptive`,
`congestion-limiter`) are bound to async HTTP / Tower / reqwest and do not fit a
rayon `par_iter` file-read loop. Decision: **port the algorithm, not a framework** —
a small self-contained controller (~dozens of lines).

Sources: Vector "Adaptive Request Concurrency"; Netflix/concurrency-limits;
rate_limiter_aimd; tower-resilience-adaptive; congestion-limiter; jpeg-decoder /
zune-jpeg / image docs.rs.

## Global principles (honored throughout)

- **Never block the UI/update thread.** All decode/read/measurement runs on
  `ferrolite-jobs` / rayon ingest workers (CLAUDE.md rule 1).
- **Zero-overhead diagnostics when off.** Every added timing/counter is gated behind
  `measure` / `diag::enabled()`; no `Instant` or allocation when the flag is off.
- **Immutability / small focused modules** per repo conventions.
- After the automated gate is green (`cargo fmt --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace`), **HOLD for the author's
  instrumented re-test** before finishing the branch (CLAUDE.md).

---

## Improvement A (PRIMARY) — downscale-decode standard-JPEG thumbnails

**Goal.** Decode JPEGs on the ingest thumbnail path at a DCT-reduced scale landing
just ≥ 256 px, then resize as today. Target ~500 ms → ~50 ms per JPEG.

**New dependency.** `jpeg-decoder = "0.3"` as a direct dep of `ferrolite-decode`
(already transitive; pure-Rust, no build risk).

**New function** (standard.rs; `decode_preview_standard` untouched):

```rust
pub struct StdThumbDecode {
    pub image: ImageBuffer, // RGB8, EXIF-oriented
    pub dct_scale: u8,      // 1 | 2 | 4 | 8 (JPEG); 1 for fallback
    pub decoded_w: u32,     // pre-resize decoded dims
    pub decoded_h: u32,
}

pub fn decode_thumb_source_standard(
    path: &Path,
    target_edge: u32,
    measure: bool,
) -> Result<StdThumbDecode, DecodeError>;
```

- **JPEG** (extension sniff + magic `FF D8 FF`): `jpeg_decoder::Decoder`, call
  `scale(target_edge, target_edge)` before `decode()`. Convert output per
  `info().pixel_format` (RGB / YCbCr→RGB / L8→RGB) to RGB8. Record `dct_scale` from
  the returned reduced dims vs full dims.
- **EXIF orientation preserved.** Read orientation via the existing `read_exif`
  helper and apply `apply_orientation` exactly as `decode_preview_standard` does
  today (jpeg-decoder returns raw pixels; we orient ourselves — unchanged behavior).
- **Non-JPEG** (PNG/WebP/BMP/GIF/TIFF): fall back to `image::open` + full decode +
  orient (today's `decode_preview_standard` body); `dct_scale = 1`.
- **Color correctness.** Output feeds the existing `generate_thumbnail`
  (fast_image_resize Lanczos3 → JPEG q85 sRGB). DCT scaling is visually lossless at
  thumbnail size; no color-management change.

**Wiring.**
- `decode_meta_and_preview` Standard arm calls `decode_thumb_source_standard(path,
  THUMB_MAX_EDGE, measure)`. To avoid a `ferrolite-decode → ferrolite-catalog`
  dependency, `target_edge` is passed in by the ingest layer (which owns the 256
  constant via `THUMB_MAX_EDGE`); `ferrolite-decode` receives a plain `u32`.
- Thumbnail-regen job (ingest.rs:725) switches to the same downscaled path.
- **Viewer (viewer/load.rs:59) and export (batch.rs:105) keep calling
  `decode_preview` → full-res. Untouched.**
- `read_metadata_standard` unchanged.

**Diag extension (zero-overhead off).** Add to `PreviewInfo` + `SlowSample`:
`std_decode_ms` (the decode call) and `dct_scale: u8` (+ decoded dims).
`format_slow_line` gains a real standard-decode stage (no longer dumped in `rest`)
and a `[dct 1/N]` tag. `[ingest-summary]` per-kind std p50/p95 (already present) will
reflect the win.

**Testing.**
- JPEG fixture decodes at expected `dct_scale`, dims ≥ target on ≥1 axis.
- EXIF-rotated JPEG fixture: orientation correctly applied (compare against
  `decode_preview_standard` orientation).
- PNG fixture: falls back at `dct_scale = 1`, correct pixels.
- Output feeds `generate_thumbnail` to ≤ 256 px.
- Regression guard: `decode_preview` (viewer/export path) still returns full-res.

---

## Improvement B (secondary) — offset-directed RAW read

**Goal.** In `with_ingest_source`, replace the blind 1→2/4/8 MiB grow tiers with a
targeted read: parse the TIFF structure in the initial prefix, locate the embedded
preview byte range (+ metadata IFDs), read exactly the minimal spanning range.
Expect ~3.7 GB → ~2.3 GB bytes-read for RAW libraries; modest wall-clock win (much
per-file cost is fixed open latency). Low value for a JPEG-only library.

**Approach — parse IFD0 from the prefix, fall back to tiers.**
1. Read the initial 1 MiB prefix (as today).
2. Parse TIFF header (`II`/`MM` + magic 42), walk IFD0 / SubIFDs for embedded-preview
   pointers: `JPEGInterchangeFormat` (513) + `JPEGInterchangeFormatLength` (514),
   and/or `StripOffsets`/`StripByteCounts`. Note EXIF/maker-note IFD offsets that
   `raw_metadata` needs.
3. Compute the minimal spanning byte range `[min(offset), max(offset+len)]` covering
   preview + metadata IFDs.
4. If it fits in the prefix → done (no extra read). Otherwise one targeted read to
   exactly that end offset. Hand the assembled buffer to rawler via
   `RawSource::new_from_slice` as today.
5. **Fallback:** if header/IFD parse fails, offsets are implausible (out of bounds /
   zero length), or the format isn't a clean TIFF-preview layout (e.g. CR3) → use the
   existing tiered incremental read unchanged. Correctness never depends on parsing.

**Crate choice.** Research `rawler`'s own IFD parser first (it already parses TIFF
internally; reuse avoids a second TIFF dep and keeps behavior consistent). If rawler
doesn't expose a cheap IFD walk over a byte slice, evaluate a lightweight standalone
(`quickexif` / `tiff`). Decision recorded in the plan after a focused API check —
no hand-rolled TIFF parsing if rawler exposes it.

**Diag.** New `SourceKind::Directed` (alongside Prefix/Grown/Full) so
`[ingest-source]` and the slow-line tier tag distinguish offset-parse success vs tier
fallback. `source_bytes` already reports actual bytes read → directly confirms the
reduction.

**Testing.** RAW fixtures (existing RAW tests skip gracefully when absent): parsed
span ⊆ file; decoded preview identical to the tiered path; corrupt/short-header input
falls back cleanly. No change for standard files.

**Risk.** RAW formats vary in preview location (CR2/CR3/NEF/ARW/RAF); CR3 is not
classic TIFF. The fallback makes this safe — worst case reads the same bytes as
today. Offset-parsing scoped to common TIFF-preview families; everything else uses
tiers.

---

## Improvement C (secondary, novel) — adaptive read-concurrency controller

**Goal.** Replace "all ~10 rayon workers hammer one device" with a controller that
measures the media's live read behavior during ingest and continuously tunes the
number of concurrent reads to the device's throughput sweet spot. No manual config;
works for any storage (SD/SSD/network) — a product feature. Never blocks the UI
thread (all on ingest workers).

**Algorithm — ported gradient controller (Netflix/Vector).**
- Track `rtt_min` (fastest observed per-read latency = no-load baseline) and a short
  rolling window of recent read latencies.
- Periodically compute `gradient = rtt_min / rtt_recent` (∈ ~(0,1]) and update:
  `new_limit = current_limit · gradient + queue_allowance` — additive-increase while
  reads stay fast, multiplicative-decrease when latency climbs. Clamp to
  `[1, hw_worker_count]`.
- Enforced by a **dynamically-resizable read-permit pool** (counting semaphore whose
  capacity the controller raises/lowers). Each worker acquires a permit around the
  *byte-read only* (`with_ingest_source` read for RAW; file read for standard), does
  CPU work (decode/resize/encode) permit-free, releases.
- Measurement covers the read segment only (reuses the `source_acquire`/read timing),
  so the control signal is device latency, not CPU.

**Structure.**
- New small module (e.g. `ferrolite-app/src/ingest/read_gate.rs`, or a
  `ferrolite-jobs` helper): `AdaptiveReadGate { permit(), record(latency, bytes),
  limit() }`, backed by atomics + a light mutex-guarded controller state. No per-read
  allocation.
- Ingest keeps `par_iter().for_each_with(...)`; each worker calls `gate.permit()`
  before the read. rayon still owns threads; the gate bounds *concurrent reads*,
  decoupling read-parallelism from CPU-parallelism (important once A makes JPEG decode
  cheap and the workload read-bound — CPU decode still uses all cores).

**Bootstrapping & safety.**
- Start conservative (e.g. limit 4), probe upward; converges within the first dozens
  of files. Fast internal storage climbs to the worker count (≈ today; no regression).
  Contended SD settles lower only if that is genuinely faster.
- **Override:** `FERROLITE_INGEST_READ_CONCURRENCY=N` pins the limit (disables
  adaptation) to A/B fixed 2/3/4/6 vs adaptive on real media. `0`/unset = adaptive.

**Diag (zero-overhead off).** New `[ingest-concurrency]` line + F9 counters: current
limit over time, `rtt_min`, rolling `rtt`, gradient, reads-in-flight histogram — watch
it converge and confirm it beats a fixed pool. Feeds `[ingest-summary]`.

**Testing.** Controller as a pure function over synthetic latency sequences (rising
latency → limit shrinks; flat-fast → grows; clamps at bounds; env override pins).
No device needed for CI. Real-media win confirmed in the author's instrumented re-test.

**Honest caveat.** Whether capping concurrency helps is genuinely uncertain (the card
may prefer full parallelism). The controller *discovers* this per-device rather than
assuming. If adaptive never beats unbounded on the author's card, the measured result
decides whether the shipped default is adaptive or effectively-unbounded. Ship only
what the numbers justify.

---

## Deliverables & gate

1. Implement A, then B, then C (subagent-driven-development, task-by-task).
2. Automated gate green: `cargo fmt --check`; `cargo clippy --workspace --all-targets
   -- -D warnings`; `cargo test --workspace`.
   - Windows build note: `cargo test --workspace` can deadlock on a stale
     `target/debug` lock, and LNK1104 (`ferrolite_app-<hash>.exe` locked) can occur —
     use an isolated `CARGO_TARGET_DIR` workaround.
3. **HOLD** for the author's instrumented re-test (`FERROLITE_DIAG=1`): baseline the
   JPEG-only library, then confirm A's speedup (`[ingest-summary]` per-kind std p50 +
   total, standard `[ingest-slow]` lines with `[dct 1/N]`), B's bytes-read reduction
   (`[ingest-source]`, `Directed` tier), and C's convergence (`[ingest-concurrency]`).
4. Only after visual/instrumented confirmation: finish the branch (local merge to
   main; no push unless requested). Git attribution disabled — no Co-Authored-By.
