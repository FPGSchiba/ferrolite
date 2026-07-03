# Spec: Thumbnail Generation Diagnostics (fold into FERROLITE_DIAG)

- **Date:** 2026-07-03
- **Branch:** `feat/thumbnail-gen-diagnostics` (off `main` @ `8e72957`, which now includes rounds 1–4 + the `FERROLITE_DIAG` dev-mode + the re-submit-storm fix)
- **Status:** design approved; ready for implementation plan

## Goal

Extend the `FERROLITE_DIAG` observability dev-mode so the **thumbnail generation
(ingest) bottleneck** becomes unambiguous. On the author's ~3320-image (≈2730
processed) RAW library, ingest takes **5–10 minutes** (≈5–10 images/sec — far
slower than parallel decode should allow), and the current instrumentation
cannot say why. This work is **observability only** — no generation fix. After
it lands, an instrumented run's `[ingest-summary]` is read to locate the cause.

## Why the current instrumentation is insufficient

The ingest job ([ferrolite-app/src/ingest.rs](../../../ferrolite-app/src/ingest.rs))
runs these phases, mostly in sequence, but only the middle one is timed:

1. `scan_tree` — folder walk (single-threaded) — **not timed**
2. **Phase A** — `insert_pending` per file in a **serial loop**, each taking the
   writer lock (≈3k serial DB inserts) — **not timed**
3. **`needs_reingest` filter** — a read-pool query **per file**, serial (≈3k
   serial reads) — **not timed**
4. **Producer** — rayon `par_iter`: `decode_meta_and_preview` +
   `generate_thumbnail` per file — timed, but only as **per-file averages**
5. **Consumer** — batches 128 rows, commits under the writer lock — timed per
   batch

The existing `FERROLITE_PROFILE_THUMBS` reports average decode/encode/upsert ms.
It cannot reveal the three most likely places the 5–10 min hides:

- **The serial phases (scan / Phase A / filter) are completely uninstrumented.**
- **Whether the parallel decode is actually parallel** (averages hide the
  achieved rayon speedup — is it N× or ~1× from contention?).
- **Producer-vs-consumer lag** — the channel is unbounded, so a slow
  consumer/DB tail is invisible (the producer just finishes early and rows pile
  up).

The diagnostic gap is **phase wall-clock + parallelism achieved + producer/
consumer lag + per-file distribution**, not more per-file averages.

## Design decisions (settled in brainstorming)

1. **Fold into `FERROLITE_DIAG`** — one unified diagnostics system; do not add a
   third flag.
2. **Remove `thumb_profile.rs` / `FERROLITE_PROFILE_THUMBS` entirely** and fold
   all of its information into `diag.rs` — no profiling capability is dropped,
   only relocated.
3. **Output = one-shot per-ingest `[ingest-summary]` + a live `ingest:` line**
   folded into the existing 1/sec log + overlay.

## Non-goals

- No generation fix (no change to scan / Phase A / filter / decode / consumer
  logic, batch size, or rayon usage). Observability only.
- No change to the scroll/UI/shutdown `FERROLITE_DIAG` behaviour already shipped.
- No `ferrolite-jobs` / `ferrolite-vt` / `ferrolite-decode` / `ferrolite-catalog`
  changes.
- No new crate dependencies.

## Architecture (`thumb_profile` removed, folded into `diag`)

`diag.rs` becomes the single home for all thumbnail/ingest timing.

- **Delete** `ferrolite-app/src/thumb_profile.rs`; drop `mod thumb_profile;`
  (main.rs) and `pub mod thumb_profile;` (lib.rs); remove `FERROLITE_PROFILE_THUMBS`.
  Everything gates on `FERROLITE_DIAG` via `diag::enabled()`.
- **Per-job `IngestProfile`** (in `diag.rs`): an `Arc`-shared struct created
  **only when `diag::enabled()`** and threaded through `ingest_job` as
  `Option<Arc<IngestProfile>>`. It holds:
  - lock-free atomics: `decode_sum_us`, `decode_max_us`, `decode_count`,
    `encode_sum_us`, `encode_count`, `upsert_sum_us`, `upsert_batches`,
    `chan_depth_max`; per-kind decode counters (RAW vs Standard).
  - a `Mutex<Vec<u32>>` of per-file decode µs (for p50/p95) — touched only when
    profiling is on; a brief push per file (negligible vs a ~200 ms decode).
  - plain phase-duration fields (`scan`, `phase_a`, `filter`, `consumer_tail`)
    and producer/consumer done instants, set by the single-threaded parts.
  - `record_decode`/`record_encode`/`record_upsert`/`note_chan_depth` methods
    are **pure accumulators** (no internal global gate) → unit-testable directly.
- **Gating:** `ingest_job` resolves `diag::enabled()` once; when off, no
  `IngestProfile` is created and every record site is skipped
  (`if let Some(p) = &profile { … }`). Zero overhead when off.
- **Summary emission:** at each job's end, `ingest_job` builds an
  `IngestSummary` from its `IngestProfile` and calls
  `diag::emit_ingest_summary(&summary)` (writes via the existing best-effort
  stderr+file sink `diag::write_log`). One summary per ingest job.
- **Live line:** a small best-effort global snapshot (current-phase enum,
  in-phase progress counter, running decode p50, channel depth) the active job
  updates; the UI-thread `DiagState::tick` reads it and appends an `ingest: …`
  line to the existing 1/sec log + overlay. Last-writer-wins across concurrent
  jobs (fine for a progress line).
- **Headless path preserved:** `measure_read` + the io/decode/encode/write
  breakdown move into `diag.rs` (gated by `diag::enabled()`);
  `thumbnail_blocking` calls the `diag` equivalents; `bench_browse` runs with
  `FERROLITE_DIAG=1` (was `FERROLITE_PROFILE_THUMBS=1`). The `ingest_tree`
  integration test only uses `thumbnail_blocking`'s return value and is
  unaffected (profiling is a no-op when the flag is off).
- The `thumb_profile::diag(...)` per-sec call at app.rs is removed; its numbers
  fold into the live `ingest:` line.

## What gets measured (→ ingest.rs points)

- **Phase wall-clock:** `scan_tree`; the Phase A serial `insert_pending` loop;
  the `needs_reingest` filter; the producer `par_iter` wall-clock; the consumer
  join tail.
- **Decode parallelism:** `decode_sum_us` (Σ per-file) ÷ producer wall-clock =
  achieved speedup, related to `std::thread::available_parallelism()`.
- **Per-file distribution:** decode p50 / p95 / max, split **per kind** (RAW vs
  Standard).
- **Encode:** Σ + avg.
- **Consumer / DB:** upsert Σ, batch count, avg per batch; **producer-done vs
  consumer-done lag**.
- **Channel depth:** max rows queued.

## Output formats

### One-shot `[ingest-summary]` (per ingest job, at job end before `IngestDone`)

```
[ingest-summary] 2730 files in 412.3s
 phases  scan 3.1s  phaseA 45.2s  filter 38.4s  decode(par) 310.7s  consumer-tail 8.2s
 decode  Σ 2100s / 10 cores → 6.8x speedup | p50 210ms p95 800ms max 3.1s
 encode  Σ 180s  avg 66ms
 upsert  21 batches  avg 380ms (Σ 8.0s)
 channel max depth 640 | producer done@320.1s  consumer done@412.3s (lag 92.2s)
 by kind  RAW 2600 (decode p50 230ms) | std 130 (decode p50 12ms)
```

### Live `ingest:` line (folded into the existing 1/sec log + overlay while ingesting)

```
 ingest  phase filter 512/2730  decode p50 210ms  chan 640  active 1
```

### Headless `[thumb-blocking]` line (bench_browse under `FERROLITE_DIAG=1`)

The folded-in `thumbnail_blocking` io/decode/encode/write breakdown, content
unchanged from today's `[thumb-profile]` line, printed every N files.

## Testing

- **Pure formatters (unit-tested with explicit inputs):**
  `format_ingest_summary(&IngestSummary)` contains the phase, speedup,
  p50/p95/max, lag, and by-kind fields; the live-line formatter; the
  `[thumb-blocking]` formatter.
- **Helpers:** percentile calc (`p50`/`p95` from a `Vec<u32>`), decode-speedup
  calc.
- **`IngestProfile` accumulation:** per-instance struct → its record methods are
  pure accumulators, unit-tested directly (same isolation as `JobStats`; no
  global-gate flakiness).
- **Zero-overhead-off:** `ingest_job` resolves `diag::enabled()` once; off ⇒ no
  `IngestProfile` created, all record sites skipped, no live-snapshot writes, no
  summary emit.
- **Regression:** all pre-existing `ferrolite-app` tests still pass; the
  `ingest_tree` integration test still passes (uses `thumbnail_blocking`'s
  result only); `bench_browse` still builds and runs.
- **Gate green** per CLAUDE.md: `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` — then **hold for the author's instrumented run**
  (`FERROLITE_DIAG=1`, open the big folder, read `[ingest-summary]`) before
  finishing/merging.

## Build note (Windows)

If `cargo test` hits `LNK1104: cannot open ...ferrolite_app-<hash>.exe`, re-run
with an isolated `CARGO_TARGET_DIR` rather than killing the process.
