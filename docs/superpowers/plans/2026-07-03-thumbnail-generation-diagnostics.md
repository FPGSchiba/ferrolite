# Thumbnail Generation Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fold all thumbnail/ingest timing into the `FERROLITE_DIAG` module (deleting `thumb_profile.rs`) and add a per-ingest `[ingest-summary]` — phase wall-clock, decode parallel-speedup, per-file distribution, producer/consumer lag — plus a live `ingest:` line, so the 5–10 min generation bottleneck becomes unambiguous.

**Architecture:** A per-job `IngestProfile` (created only when diag is enabled, threaded as `Option<Arc<IngestProfile>>`) accumulates decode/encode/upsert timings + a per-file decode-µs sample; `ingest_job` times each phase with local `Instant`s and emits an `IngestSummary` at job end via `diag::write_log`. The headless `thumbnail_blocking` io/decode/encode/write profile and the per-second line move into `diag.rs` too. Everything gates on `diag::enabled()`; `thumb_profile.rs` and `FERROLITE_PROFILE_THUMBS` are removed.

**Tech Stack:** Rust, `std::sync::atomic`, `std::sync::Mutex`, rayon (existing). No new dependencies.

## Global Constraints

- **Observability only** — no change to generation logic (scan / Phase A / filter / decode / consumer / batch size / rayon usage).
- **Zero overhead when `FERROLITE_DIAG` is off:** `ingest_job` resolves `diag::enabled()` once; when off, no `IngestProfile` is created and every record/phase-timing/live-snapshot/summary site is skipped (`if let Some(p) = &profile { … }`).
- **Remove `thumb_profile.rs` and `FERROLITE_PROFILE_THUMBS` entirely**; fold every role into `diag.rs`. No profiling capability dropped, only relocated. `bench_browse` is run with `FERROLITE_DIAG=1` (was `FERROLITE_PROFILE_THUMBS=1`) — no code change to `bench_browse`.
- **`IngestProfile` record methods are pure accumulators** (no internal global gate) so they unit-test directly.
- `std::sync::mpsc` has no length API — track channel depth with an explicit inflight counter (inc on send, dec on recv, `fetch_max` on inc).
- No `ferrolite-jobs` / `ferrolite-vt` / `ferrolite-decode` / `ferrolite-catalog` changes. No new crate dependencies.
- Rust: `cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; 100-col; no `unwrap()`/`expect()` in non-test code.
- **Gate green then HOLD for the author's instrumented run** (`FERROLITE_DIAG=1`, open the big folder, read `[ingest-summary]`) before finishing (CLAUDE.md).
- Windows: if `cargo test` hits `LNK1104: cannot open ...ferrolite_app-<hash>.exe`, re-run with `CARGO_TARGET_DIR=target-diag`.

---

## File Structure

| File | Responsibility | Task |
|------|----------------|------|
| `ferrolite-app/src/diag.rs` | Add `IngestProfile`, `IngestSummary`, `format_ingest_summary`, percentile/speedup helpers, `emit_ingest_summary`, blocking-profile fns (`measure_read`/`record_blocking`/`format_blocking`); later the live `ingest:` line + globals | 1, 3 |
| `ferrolite-app/src/ingest.rs` | Thread `IngestProfile`, time phases, accumulate decode/encode/upsert/channel, emit summary; `thumbnail_blocking` uses diag blocking-profile; update live snapshot | 2, 3 |
| `ferrolite-app/src/app.rs` | Remove `thumb_profile::diag(...)` call; later feed the live `ingest:` gauge fields | 2, 3 |
| `ferrolite-app/src/thumb_profile.rs` | **Deleted** | 2 |
| `ferrolite-app/src/main.rs`, `lib.rs` | Drop `mod thumb_profile;` / `pub mod thumb_profile;` | 2 |

---

## Task 1: diag.rs — `IngestProfile`, `IngestSummary`, formatters, helpers, blocking profile (additive)

Purely additive to `diag.rs`. `thumb_profile.rs` stays untouched this task (removed in Task 2), so everything still compiles.

**Files:**
- Modify: `ferrolite-app/src/diag.rs`

**Interfaces:**
- Produces:
  - `struct IngestProfile` (`Default`) with methods `record_decode(&self, us: u64, is_raw: bool)`, `record_encode(&self, us: u64)`, `record_upsert(&self, us: u64)`, `on_send(&self)`, `on_recv(&self)`, and readers `decode_samples(&self) -> Vec<u32>`, `raw_samples(&self) -> Vec<u32>`, `std_samples(&self) -> Vec<u32>`, plus atomic getters used by the summary builder.
  - `struct IngestSummary { files, wall_s, scan_s, phase_a_s, filter_s, decode_par_s, decode_sum_s, cores, decode_p50_ms, decode_p95_ms, decode_max_ms, encode_sum_s, encode_avg_ms, upsert_batches, upsert_avg_ms, upsert_sum_s, chan_depth_max, producer_done_s, consumer_done_s, raw_count, raw_p50_ms, std_count, std_p50_ms }` (all `f64`/`usize`/`u64`).
  - `fn percentile(samples: &[u32], pct: f64) -> u32`
  - `fn format_ingest_summary(s: &IngestSummary) -> String`
  - `fn emit_ingest_summary(s: &IngestSummary)` (writes via `write_log`)
  - Blocking profile: `fn measure_read(path: &std::path::Path) -> u64`, `fn record_blocking(io_us: u64, decode_us: u64, encode_us: u64, write_us: u64)`, and `PROBE_READ_BYTES`.

- [ ] **Step 1: Write the failing tests**

Add to `ferrolite-app/src/diag.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn percentile_picks_expected_values() {
        let v: Vec<u32> = (1..=100).collect(); // 1..100
        assert_eq!(percentile(&v, 0.5), 50);
        assert_eq!(percentile(&v, 0.95), 95);
        assert_eq!(percentile(&v, 1.0), 100);
        assert_eq!(percentile(&[], 0.5), 0, "empty → 0");
        assert_eq!(percentile(&[7], 0.5), 7, "single element");
    }

    #[test]
    fn ingest_profile_accumulates_per_kind() {
        let p = IngestProfile::default();
        p.record_decode(100_000, true); // 100ms RAW
        p.record_decode(300_000, true); // 300ms RAW
        p.record_decode(10_000, false); // 10ms std
        p.record_encode(40_000);
        p.record_upsert(380_000);
        p.on_send();
        p.on_send();
        p.on_recv();
        assert_eq!(p.decode_samples().len(), 3);
        assert_eq!(p.raw_samples().len(), 2);
        assert_eq!(p.std_samples().len(), 1);
        assert_eq!(p.decode_sum_us(), 410_000);
        assert_eq!(p.decode_max_us(), 300_000);
        assert_eq!(p.encode_sum_us(), 40_000);
        assert_eq!(p.upsert_sum_us(), 380_000);
        assert_eq!(p.upsert_batches(), 1);
        assert_eq!(p.chan_depth_max(), 2, "peak inflight was 2 before the recv");
    }

    #[test]
    fn format_ingest_summary_contains_all_sections() {
        let s = IngestSummary {
            files: 2730,
            wall_s: 412.3,
            scan_s: 3.1,
            phase_a_s: 45.2,
            filter_s: 38.4,
            decode_par_s: 310.7,
            decode_sum_s: 2100.0,
            cores: 10,
            decode_p50_ms: 210.0,
            decode_p95_ms: 800.0,
            decode_max_ms: 3100.0,
            encode_sum_s: 180.0,
            encode_avg_ms: 66.0,
            upsert_batches: 21,
            upsert_avg_ms: 380.0,
            upsert_sum_s: 8.0,
            chan_depth_max: 640,
            producer_done_s: 320.1,
            consumer_done_s: 412.3,
            raw_count: 2600,
            raw_p50_ms: 230.0,
            std_count: 130,
            std_p50_ms: 12.0,
        };
        let out = format_ingest_summary(&s);
        assert!(out.contains("[ingest-summary] 2730 files in 412.3s"));
        assert!(out.contains("scan 3.1s"));
        assert!(out.contains("phaseA 45.2s"));
        assert!(out.contains("filter 38.4s"));
        assert!(out.contains("decode(par) 310.7s"));
        assert!(out.contains("6.8x"), "speedup = 2100/310.7 ≈ 6.8");
        assert!(out.contains("p50 210ms p95 800ms max 3100ms"));
        assert!(out.contains("tail 92.2s"), "consumer_done - producer_done");
        assert!(out.contains("RAW 2600"));
        assert!(out.contains("std 130"));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p ferrolite-app diag::tests::percentile_picks_expected_values diag::tests::ingest_profile_accumulates_per_kind diag::tests::format_ingest_summary_contains_all_sections`
Expected: FAIL — the types/functions don't exist.

- [ ] **Step 3: Implement `IngestProfile` + getters**

Append to `ferrolite-app/src/diag.rs` (above the `#[cfg(test)]` module). Add `use std::sync::Arc;` and `use std::path::Path;` to the top imports if not present (Arc is used by the summary path; Path by `measure_read`).

```rust
/// Per-ingest-job generation profile. Created only when `diag::enabled()`,
/// threaded through `ingest_job` as `Option<Arc<IngestProfile>>`. All methods
/// are pure accumulators (no global gate) so they unit-test directly; the caller
/// gates creation/use behind `enabled()`. `Relaxed` throughout — diagnostics,
/// not synchronization.
#[derive(Default)]
pub struct IngestProfile {
    decode_sum_us: AtomicU64,
    decode_max_us: AtomicU64,
    encode_sum_us: AtomicU64,
    encode_count: AtomicU64,
    upsert_sum_us: AtomicU64,
    upsert_batches: AtomicU64,
    chan_inflight: AtomicU64,
    chan_depth_max: AtomicU64,
    // Per-file decode µs, split by kind (RAW vs Standard) for per-kind p50; the
    // overall distribution is the two merged. Touched only when profiling is on;
    // a brief push per file, negligible vs a ~200ms decode.
    raw_us: Mutex<Vec<u32>>,
    std_us: Mutex<Vec<u32>>,
}

impl IngestProfile {
    pub fn record_decode(&self, us: u64, is_raw: bool) {
        self.decode_sum_us.fetch_add(us, Ordering::Relaxed);
        self.decode_max_us.fetch_max(us, Ordering::Relaxed);
        let bucket = if is_raw { &self.raw_us } else { &self.std_us };
        if let Ok(mut v) = bucket.lock() {
            v.push(us as u32);
        }
    }
    pub fn record_encode(&self, us: u64) {
        self.encode_sum_us.fetch_add(us, Ordering::Relaxed);
        self.encode_count.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_upsert(&self, us: u64) {
        self.upsert_sum_us.fetch_add(us, Ordering::Relaxed);
        self.upsert_batches.fetch_add(1, Ordering::Relaxed);
    }
    /// Producer sent a row into the channel: bump inflight and track the peak.
    pub fn on_send(&self) {
        let depth = self.chan_inflight.fetch_add(1, Ordering::Relaxed) + 1;
        self.chan_depth_max.fetch_max(depth, Ordering::Relaxed);
    }
    /// Consumer took a row off the channel.
    pub fn on_recv(&self) {
        self.chan_inflight.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn decode_sum_us(&self) -> u64 {
        self.decode_sum_us.load(Ordering::Relaxed)
    }
    pub fn decode_max_us(&self) -> u64 {
        self.decode_max_us.load(Ordering::Relaxed)
    }
    pub fn encode_sum_us(&self) -> u64 {
        self.encode_sum_us.load(Ordering::Relaxed)
    }
    pub fn upsert_sum_us(&self) -> u64 {
        self.upsert_sum_us.load(Ordering::Relaxed)
    }
    pub fn upsert_batches(&self) -> u64 {
        self.upsert_batches.load(Ordering::Relaxed)
    }
    pub fn chan_depth_max(&self) -> u64 {
        self.chan_depth_max.load(Ordering::Relaxed)
    }
    /// All per-file decode samples (RAW ∪ Standard), for overall percentiles.
    pub fn decode_samples(&self) -> Vec<u32> {
        let mut out = self.raw_samples();
        out.extend(self.std_samples());
        out
    }
    pub fn raw_samples(&self) -> Vec<u32> {
        self.raw_us.lock().map(|v| v.clone()).unwrap_or_default()
    }
    pub fn std_samples(&self) -> Vec<u32> {
        self.std_us.lock().map(|v| v.clone()).unwrap_or_default()
    }
}
```

- [ ] **Step 4: Implement `percentile`, `IngestSummary`, `format_ingest_summary`, `emit_ingest_summary`**

Append to `ferrolite-app/src/diag.rs`:

```rust
/// Nearest-rank percentile of `samples` (µs). Returns 0 for an empty slice.
/// `pct` in 0.0..=1.0. Clones + sorts, so callers pass a snapshot, not a hot Vec.
pub fn percentile(samples: &[u32], pct: f64) -> u32 {
    if samples.is_empty() {
        return 0;
    }
    let mut v = samples.to_vec();
    v.sort_unstable();
    let rank = (pct * v.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(v.len() - 1);
    v[idx]
}

/// Plain, `format`-ready snapshot of one ingest job's generation profile.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct IngestSummary {
    pub files: usize,
    pub wall_s: f64,
    pub scan_s: f64,
    pub phase_a_s: f64,
    pub filter_s: f64,
    pub decode_par_s: f64,
    pub decode_sum_s: f64,
    pub cores: usize,
    pub decode_p50_ms: f64,
    pub decode_p95_ms: f64,
    pub decode_max_ms: f64,
    pub encode_sum_s: f64,
    pub encode_avg_ms: f64,
    pub upsert_batches: u64,
    pub upsert_avg_ms: f64,
    pub upsert_sum_s: f64,
    pub chan_depth_max: u64,
    pub producer_done_s: f64,
    pub consumer_done_s: f64,
    pub raw_count: usize,
    pub raw_p50_ms: f64,
    pub std_count: usize,
    pub std_p50_ms: f64,
}

/// Render the one-shot per-ingest summary block.
pub fn format_ingest_summary(s: &IngestSummary) -> String {
    let speedup = if s.decode_par_s > 0.0 {
        s.decode_sum_s / s.decode_par_s
    } else {
        0.0
    };
    let tail = (s.consumer_done_s - s.producer_done_s).max(0.0);
    format!(
        "[ingest-summary] {files} files in {wall:.1}s\n\
         \x20phases  scan {scan:.1}s  phaseA {pa:.1}s  filter {flt:.1}s  decode(par) {dec:.1}s\n\
         \x20decode  \u{03a3} {dsum:.0}s / {cores} cores \u{2192} {sp:.1}x | p50 {p50:.0}ms p95 {p95:.0}ms max {mx:.0}ms\n\
         \x20encode  \u{03a3} {esum:.1}s  avg {eavg:.0}ms\n\
         \x20upsert  {ub} batches  avg {uavg:.0}ms (\u{03a3} {usum:.1}s)\n\
         \x20channel max depth {chan}  | producer done@{pd:.1}s  consumer done@{cd:.1}s  (tail {tail:.1}s)\n\
         \x20by kind  RAW {rawn} (decode p50 {rawp50:.0}ms) | std {stdn} (decode p50 {stdp50:.0}ms)",
        files = s.files,
        wall = s.wall_s,
        scan = s.scan_s,
        pa = s.phase_a_s,
        flt = s.filter_s,
        dec = s.decode_par_s,
        dsum = s.decode_sum_s,
        cores = s.cores,
        sp = speedup,
        p50 = s.decode_p50_ms,
        p95 = s.decode_p95_ms,
        mx = s.decode_max_ms,
        esum = s.encode_sum_s,
        eavg = s.encode_avg_ms,
        ub = s.upsert_batches,
        uavg = s.upsert_avg_ms,
        usum = s.upsert_sum_s,
        chan = s.chan_depth_max,
        pd = s.producer_done_s,
        cd = s.consumer_done_s,
        tail = tail,
        rawn = s.raw_count,
        rawp50 = s.raw_p50_ms,
        stdn = s.std_count,
        stdp50 = s.std_p50_ms,
    )
}

/// Emit the ingest summary to the diag log sink (stderr + session file).
/// Gated: no-op when diag is off.
pub fn emit_ingest_summary(s: &IngestSummary) {
    if !enabled() {
        return;
    }
    write_log(&format_ingest_summary(s));
}
```

- [ ] **Step 5: Fold in the headless blocking profile (`measure_read` + `record_blocking`)**

Append to `ferrolite-app/src/diag.rs` (this replaces `thumb_profile`'s `measure_read`/`record`/`PROBE_READ_BYTES`/`SUMMARY_EVERY` and their statics, relocated verbatim in spirit):

```rust
/// Bytes pre-read to force + time the disk IO a preview decode pages in (headless
/// `thumbnail_blocking` bench path). ~2 MiB covers the embedded preview prefix.
const PROBE_READ_BYTES: usize = 2 << 20;
/// Emit a running blocking-profile summary every this many profiled thumbnails.
const BLOCKING_SUMMARY_EVERY: u64 = 2;

static BLK_COUNT: AtomicU64 = AtomicU64::new(0);
static BLK_IO_US: AtomicU64 = AtomicU64::new(0);
static BLK_DECODE_US: AtomicU64 = AtomicU64::new(0);
static BLK_ENCODE_US: AtomicU64 = AtomicU64::new(0);
static BLK_WRITE_US: AtomicU64 = AtomicU64::new(0);
static BLK_READ_BYTES: AtomicU64 = AtomicU64::new(0);

/// Force + time the cold disk read for `path` (also warms the OS cache so the
/// decode timed next reflects CPU only). Returns the read duration in µs. Used
/// only by `ingest::thumbnail_blocking` (bench/test). Callers gate on `enabled()`.
pub fn measure_read(path: &Path) -> u64 {
    use std::io::Read;
    let t = Instant::now();
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut buf = vec![0u8; PROBE_READ_BYTES];
        if let Ok(n) = f.read(&mut buf) {
            BLK_READ_BYTES.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
    t.elapsed().as_micros() as u64
}

/// Record one headless blocking-thumbnail's phase timings (µs) and print a
/// cumulative `[thumb-blocking]` summary every `BLOCKING_SUMMARY_EVERY` files.
/// Gated: no-op when diag is off.
pub fn record_blocking(io_us: u64, decode_us: u64, encode_us: u64, write_us: u64) {
    if !enabled() {
        return;
    }
    let n = BLK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let io = BLK_IO_US.fetch_add(io_us, Ordering::Relaxed) + io_us;
    let dec = BLK_DECODE_US.fetch_add(decode_us, Ordering::Relaxed) + decode_us;
    let enc = BLK_ENCODE_US.fetch_add(encode_us, Ordering::Relaxed) + encode_us;
    let wr = BLK_WRITE_US.fetch_add(write_us, Ordering::Relaxed) + write_us;
    if !n.is_multiple_of(BLOCKING_SUMMARY_EVERY) {
        return;
    }
    let bytes = BLK_READ_BYTES.load(Ordering::Relaxed);
    let mbps = if io > 0 { bytes as f64 / io as f64 } else { 0.0 };
    let nf = n as f64;
    write_log(&format!(
        "[thumb-blocking] n={n}  avg/file: io={:.1}ms decode={:.1}ms encode={:.1}ms write={:.1}ms \
         | read {:.0}MB @ {:.1}MB/s",
        io as f64 / 1000.0 / nf,
        dec as f64 / 1000.0 / nf,
        enc as f64 / 1000.0 / nf,
        wr as f64 / 1000.0 / nf,
        bytes as f64 / 1e6,
        mbps,
    ));
}
```

- [ ] **Step 6: Run the tests + gate**

Run: `cargo test -p ferrolite-app diag::tests`
Expected: the three new tests pass; existing diag tests still pass.

Run: `cargo fmt -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean. (New items unused until Task 2 — if clippy flags dead code on `measure_read`/`record_blocking`/`emit_ingest_summary`/`IngestProfile`, add a per-item `#[allow(dead_code)]` with a "wired in Task 2" note; remove them in Task 2.)

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/diag.rs
git commit -m "diag(app): IngestProfile, ingest summary, percentile/blocking helpers"
```

---

## Task 2: Rewire ingest.rs + thumbnail_blocking to diag; delete thumb_profile

Replaces every `thumb_profile` use with the Task-1 diag machinery, wires phase timing + the summary into `ingest_job`, and deletes `thumb_profile.rs`.

**Files:**
- Modify: `ferrolite-app/src/ingest.rs`, `ferrolite-app/src/app.rs`, `ferrolite-app/src/main.rs`, `ferrolite-app/src/lib.rs`
- Delete: `ferrolite-app/src/thumb_profile.rs`

**Interfaces:**
- Consumes: everything from Task 1 (`IngestProfile`, `IngestSummary`, `percentile`, `emit_ingest_summary`, `measure_read`, `record_blocking`, `enabled`).

- [ ] **Step 1: Thread `IngestProfile` + phase timing into `ingest_job`**

In `ferrolite-app/src/ingest.rs`, `ingest_job` (starts line 277). At the very top of the function body (after the signature), add the profile + job clock:

```rust
    let profile = crate::diag::enabled().then(|| std::sync::Arc::new(crate::diag::IngestProfile::default()));
    let t_job = profile.as_ref().map(|_| std::time::Instant::now());
    // Phase wall-clocks (seconds), filled when profiling.
    let mut scan_s = 0.0f64;
    let mut phase_a_s = 0.0f64;
    let mut filter_s = 0.0f64;
    let mut producer_done_s = 0.0f64;
    let mut file_count = 0usize;
```

Wrap `scan_tree`:

```rust
    let t_scan = profile.as_ref().map(|_| std::time::Instant::now());
    let files = scan_tree(&folder);
    if let Some(t) = t_scan {
        scan_s = t.elapsed().as_secs_f64();
    }
    file_count = files.len();
```

Wrap the Phase A block: capture an `Instant` immediately before the `{ let added_at = now_epoch_secs(); … }` Phase A scope (line ~315) and record after it closes (line ~351):

```rust
    let t_phase_a = profile.as_ref().map(|_| std::time::Instant::now());
    {
        // ... existing Phase A body unchanged ...
    }
    if let Some(t) = t_phase_a {
        phase_a_s = t.elapsed().as_secs_f64();
    }
```

- [ ] **Step 2: Time the filter, thread profile into producer + consumer, record producer-done**

Still in `ingest_job`, inside the `std::thread::scope`:

Time the `to_process` build (line ~422): wrap the `let to_process: Vec<...> = files.iter()...collect();` with an `Instant` and set `filter_s` after (place the write after the `collect()` and before the `IngestPlanned` send):

```rust
        let t_filter = profile.as_ref().map(|_| std::time::Instant::now());
        let to_process: Vec<(&ferrolite_catalog::ScannedFile, i64)> = files
            .iter()
            .filter_map(|f| { /* unchanged */ })
            .collect();
        if let Some(t) = t_filter {
            filter_s = t.elapsed().as_secs_f64();
        }
        let _ = tx.send(AppEvent::IngestPlanned { total: to_process.len() });
```

In the **consumer** closure, clone the profile in and thread it into `flush_batch` + `on_recv`. Change the consumer's captured clones to include:

```rust
        let consumer = {
            let writer = Arc::clone(&writer);
            let tx = tx.clone();
            let ctx = ctx.clone();
            let profile = profile.clone();
            scope.spawn(move || {
                // ... unchanged setup ...
                loop {
                    if cancel.is_cancelled() {
                        break;
                    }
                    match row_rx.recv_timeout(FLUSH_INTERVAL) {
                        Ok(row) => {
                            if let Some(p) = &profile {
                                p.on_recv();
                            }
                            pending.push(row);
                            if pending.len() >= INGEST_WRITE_BATCH {
                                flush_batch(&writer, &tx, &ctx, force, &mut pending, &mut kept, profile.as_ref());
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            flush_batch(&writer, &tx, &ctx, force, &mut pending, &mut kept, profile.as_ref());
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                flush_batch(&writer, &tx, &ctx, force, &mut pending, &mut kept, profile.as_ref());
                kept
            })
        };
```

In the **producer** (`to_process.par_iter().for_each_with(row_tx, …)`), wrap the whole `par_iter` in a producer clock, and inside the closure record decode/encode + `on_send`. First, capture the producer start immediately before `to_process.par_iter()`:

```rust
        let t_producer = profile.as_ref().map(|_| std::time::Instant::now());
        to_process
            .par_iter()
            .for_each_with(row_tx, |sender, &(f, folder_id)| {
                if cancel.is_cancelled() {
                    return;
                }
                let added_at = now_epoch_secs();
                let rating =
                    ferrolite_catalog::read_rating(&ferrolite_catalog::sidecar_path(&f.path))
                        .unwrap_or_default();
                let is_raw = matches!(f.kind, ferrolite_catalog::FileKind::Raw);
                let t_meta = profile.as_ref().map(|_| std::time::Instant::now());
                let decoded = ferrolite_decode::decode_meta_and_preview(&f.path, f.kind);
                if let (Some(t), Some(p)) = (t_meta, profile.as_ref()) {
                    p.record_decode(t.elapsed().as_micros() as u64, is_raw);
                }
                match decoded {
                    Ok((meta, preview)) => {
                        let new_image = NewImage::from_metadata(
                            folder_id, f.filename.clone(), f.mtime, f.size, &meta, f.kind, rating, added_at,
                        );
                        if cancel.is_cancelled() {
                            return;
                        }
                        let t_enc = profile.as_ref().map(|_| std::time::Instant::now());
                        let thumb = match ferrolite_catalog::generate_thumbnail(&preview) {
                            Ok(pair) => Some(pair),
                            Err(e) => {
                                eprintln!("ferrolite: thumbnail generation failed for {}: {e}", f.path.display());
                                None
                            }
                        };
                        if let (Some(t), Some(p)) = (t_enc, profile.as_ref()) {
                            p.record_encode(t.elapsed().as_micros() as u64);
                        }
                        if let Some(p) = profile.as_ref() {
                            p.on_send();
                        }
                        let _ = sender.send((new_image, thumb));
                    }
                    Err(_) => {
                        let new_image = NewImage::failed(
                            folder_id, f.filename.clone(), f.mtime, f.size, f.kind, added_at,
                        );
                        if let Some(p) = profile.as_ref() {
                            p.on_send();
                        }
                        let _ = sender.send((new_image, None));
                    }
                }
            });
        if let Some(t) = t_producer {
            producer_done_s = t.elapsed().as_secs_f64();
        }

        kept_image_ids = consumer.join().expect("ingest consumer thread panicked");
```

> Note: `producer_done_s`/`t_producer` use the `t_producer` instant (producer wall-clock), while `producer done@` in the summary is measured from `t_job`. Compute both: keep `t_producer` for `decode_par_s` (producer wall-clock) and, right after the `par_iter` returns, also stamp `producer_done_at_s = t_job.elapsed()` for the summary's `producer done@`. Add near the `producer_done_s` write:
>
> ```rust
>         let producer_done_at_s = t_job.map_or(0.0, |t| t.elapsed().as_secs_f64());
> ```
> and thread `producer_done_at_s` out of the scope (declare `let mut producer_done_at_s = 0.0f64;` beside the other phase locals before the scope, and assign inside).

Declare `producer_done_at_s` with the other phase locals in Step 1 (add `let mut producer_done_at_s = 0.0f64;`). `decode_par_s = producer_done_s` (producer wall-clock).

- [ ] **Step 3: Update `flush_batch` to record upsert timing via the profile**

In `ferrolite-app/src/ingest.rs`, change `flush_batch`'s signature and its timing (replace the `thumb_profile` usage at lines 221 + 255-257):

```rust
fn flush_batch(
    writer: &Arc<Mutex<Catalog>>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    force: bool,
    pending: &mut Vec<(NewImage, Option<(Thumbnail, DecodedThumb)>)>,
    kept: &mut HashSet<i64>,
    profile: Option<&Arc<crate::diag::IngestProfile>>,
) {
    if pending.is_empty() {
        return;
    }
    let rows = std::mem::take(pending);
    let t_batch = profile.map(|_| std::time::Instant::now());

    // ... unchanged split into decoded_thumbs / batch_input ...

    let ids = match writer
        .lock()
        .expect("writer")
        .upsert_images_with_thumbnails_batch(&batch_input)
    {
        Ok(ids) => ids,
        Err(e) => {
            eprintln!("ferrolite: batched ingest write failed ({} rows): {e}", batch_input.len());
            return;
        }
    };
    if let (Some(t), Some(p)) = (t_batch, profile) {
        p.record_upsert(t.elapsed().as_micros() as u64);
    }

    // ... unchanged per-row Indexed/ThumbReady emit + request_repaint ...
}
```

- [ ] **Step 4: Build + emit the `IngestSummary` at job end**

In `ferrolite-app/src/ingest.rs`, `ingest_job`, right before the final `let _ = tx.send(AppEvent::IngestDone);` (line ~531), add:

```rust
    if let (Some(p), Some(t)) = (&profile, t_job) {
        let wall_s = t.elapsed().as_secs_f64();
        let raw = p.raw_samples();
        let std_s = p.std_samples();
        let all = p.decode_samples();
        let us_to_ms = |u: u32| u as f64 / 1000.0;
        let encode_count = all.len().max(1) as f64;
        let summary = crate::diag::IngestSummary {
            files: file_count,
            wall_s,
            scan_s,
            phase_a_s,
            filter_s,
            decode_par_s: producer_done_s,
            decode_sum_s: p.decode_sum_us() as f64 / 1e6,
            cores: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
            decode_p50_ms: us_to_ms(crate::diag::percentile(&all, 0.5)),
            decode_p95_ms: us_to_ms(crate::diag::percentile(&all, 0.95)),
            decode_max_ms: p.decode_max_us() as f64 / 1000.0,
            encode_sum_s: p.encode_sum_us() as f64 / 1e6,
            encode_avg_ms: (p.encode_sum_us() as f64 / 1000.0) / encode_count,
            upsert_batches: p.upsert_batches(),
            upsert_avg_ms: if p.upsert_batches() > 0 {
                (p.upsert_sum_us() as f64 / 1000.0) / p.upsert_batches() as f64
            } else {
                0.0
            },
            upsert_sum_s: p.upsert_sum_us() as f64 / 1e6,
            chan_depth_max: p.chan_depth_max(),
            producer_done_s: producer_done_at_s,
            consumer_done_s: wall_s,
            raw_count: raw.len(),
            raw_p50_ms: us_to_ms(crate::diag::percentile(&raw, 0.5)),
            std_count: std_s.len(),
            std_p50_ms: us_to_ms(crate::diag::percentile(&std_s, 0.5)),
        };
        crate::diag::emit_ingest_summary(&summary);
    }
```

- [ ] **Step 5: Rewire `thumbnail_blocking` to the diag blocking profile**

In `ferrolite-app/src/ingest.rs`, `thumbnail_blocking` (line ~545), replace the `thumb_profile` calls with `diag`:

```rust
    let profile = crate::diag::enabled();
    let io_us = if profile {
        crate::diag::measure_read(path)
    } else {
        0
    };

    let t_decode = profile.then(std::time::Instant::now);
    let preview = ferrolite_decode::decode_preview(path, kind).map_err(|e| e.to_string())?;
    let decode_us = t_decode.map_or(0, |t| t.elapsed().as_micros() as u64);

    let t_encode = profile.then(std::time::Instant::now);
    let (thumb, decoded) =
        ferrolite_catalog::generate_thumbnail(&preview).map_err(|e| e.to_string())?;
    let encode_us = t_encode.map_or(0, |t| t.elapsed().as_micros() as u64);

    let t_write = profile.then(std::time::Instant::now);
    {
        use ferrolite_catalog::ThumbnailStore;
        writer
            .lock()
            .expect("writer")
            .put_thumbnail(image_id, &thumb)
            .map_err(|e| e.to_string())?;
    }
    let write_us = t_write.map_or(0, |t| t.elapsed().as_micros() as u64);

    if profile {
        crate::diag::record_blocking(io_us, decode_us, encode_us, write_us);
    }
    Ok((thumb, decoded))
```

- [ ] **Step 6: Remove the per-second `thumb_profile::diag` call in app.rs**

In `ferrolite-app/src/app.rs`, delete the block at lines 1338–1347 (the comment + the `crate::thumb_profile::diag(...)` call). (The live `ingest:` line replaces it in Task 3.)

- [ ] **Step 7: Delete `thumb_profile.rs` and its module declarations**

```bash
git rm ferrolite-app/src/thumb_profile.rs
```

In `ferrolite-app/src/main.rs`, remove the line `mod thumb_profile;` (line 16).
In `ferrolite-app/src/lib.rs`, remove the line `pub mod thumb_profile;` (line 15).
In `ferrolite-app/src/diag.rs`, update the module doc comment (lines 1–4): drop the "Sibling to `thumb_profile.rs`" clause (it no longer exists), e.g. change "the per-frame tick, and the overlay. Sibling to `thumb_profile.rs` (the narrow ingest profiler), which this does not touch." to "the per-frame tick, the overlay, and ingest/blocking generation profiling."

- [ ] **Step 8: Build + test + gate**

Run: `cargo build -p ferrolite-app && cargo test -p ferrolite-app`
Expected: compiles with zero `thumb_profile` references; all tests pass (including `tests/ingest_tree.rs`, which uses `thumbnail_blocking`'s return value only).

Run: `cargo build -p ferrolite-app --bin bench_browse`
Expected: `bench_browse` still builds (it calls `thumbnail_blocking`, unchanged signature).

Run: `cargo fmt -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean. (Remove any Task-1 `#[allow(dead_code)]` now that the items are used.)

- [ ] **Step 9: Commit**

```bash
git add ferrolite-app/src/ingest.rs ferrolite-app/src/app.rs ferrolite-app/src/main.rs ferrolite-app/src/lib.rs ferrolite-app/src/diag.rs
git commit -m "perf-diag(app): fold ingest+blocking profiling into diag, emit ingest-summary, remove thumb_profile"
```

---

## Task 3: Live `ingest:` line in the 1/sec log + overlay

Adds a best-effort live phase/progress/channel line so the overlay + 1/sec log show ingest progressing.

**Files:**
- Modify: `ferrolite-app/src/diag.rs`, `ferrolite-app/src/ingest.rs`, `ferrolite-app/src/app.rs`

**Interfaces:**
- Produces: `diag::set_ingest_phase(phase: IngestPhase)`, `diag::note_ingest_chan(depth: u64)`, `diag::ingest_phase() -> IngestPhase`, `diag::ingest_chan() -> u64`; `Gauges.ingest_phase: IngestPhase`, `Gauges.ingest_chan: u64`.

- [ ] **Step 1: Write the failing test**

Add to `ferrolite-app/src/diag.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn format_log_shows_ingest_line_when_a_phase_is_active() {
        let mut g = sample_gauges();
        g.ingest_phase = IngestPhase::Decode;
        g.ingest_chan = 512;
        g.ingest_done = 1450;
        g.ingest_total = 2730;
        let s = build_snapshot(
            1.0,
            &AppCounters::default(),
            &AppCounters::default(),
            &AppCounters::default(),
            ferrolite_jobs::JobStats::default(),
            g,
            5.0,
            5.0,
            false,
        );
        let out = format_log(&s);
        assert!(out.contains("ingest  phase decode 1450/2730"), "shows phase + progress");
        assert!(out.contains("chan 512"), "shows channel depth");
    }

    #[test]
    fn format_log_hides_ingest_line_when_idle() {
        let mut g = sample_gauges();
        g.ingest_phase = IngestPhase::Idle;
        let s = build_snapshot(
            1.0, &AppCounters::default(), &AppCounters::default(), &AppCounters::default(),
            ferrolite_jobs::JobStats::default(), g, 5.0, 5.0, false,
        );
        assert!(!format_log(&s).contains(" ingest  phase"), "no ingest line when idle");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-app diag::tests::format_log_shows_ingest_line_when_a_phase_is_active diag::tests::format_log_hides_ingest_line_when_idle`
Expected: FAIL — `IngestPhase` / gauge fields don't exist.

- [ ] **Step 3: Add `IngestPhase`, live globals, setters/getters**

In `ferrolite-app/src/diag.rs`, add the phase enum + globals (place near the other statics). `AtomicU8` needs `use std::sync::atomic::AtomicU8;` added to imports:

```rust
/// Current ingest phase, for the live `ingest:` line. `Idle` hides the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestPhase {
    Idle,
    Scan,
    PhaseA,
    Filter,
    Decode,
    Done,
}

impl IngestPhase {
    fn label(self) -> &'static str {
        match self {
            IngestPhase::Idle => "idle",
            IngestPhase::Scan => "scan",
            IngestPhase::PhaseA => "phaseA",
            IngestPhase::Filter => "filter",
            IngestPhase::Decode => "decode",
            IngestPhase::Done => "done",
        }
    }
    fn from_u8(v: u8) -> IngestPhase {
        match v {
            1 => IngestPhase::Scan,
            2 => IngestPhase::PhaseA,
            3 => IngestPhase::Filter,
            4 => IngestPhase::Decode,
            5 => IngestPhase::Done,
            _ => IngestPhase::Idle,
        }
    }
    fn as_u8(self) -> u8 {
        match self {
            IngestPhase::Idle => 0,
            IngestPhase::Scan => 1,
            IngestPhase::PhaseA => 2,
            IngestPhase::Filter => 3,
            IngestPhase::Decode => 4,
            IngestPhase::Done => 5,
        }
    }
}

static INGEST_PHASE: AtomicU8 = AtomicU8::new(0);
static INGEST_CHAN: AtomicU64 = AtomicU64::new(0);

/// Best-effort live phase publish (last-writer-wins across concurrent jobs).
pub fn set_ingest_phase(phase: IngestPhase) {
    INGEST_PHASE.store(phase.as_u8(), Ordering::Relaxed);
}
pub fn note_ingest_chan(depth: u64) {
    INGEST_CHAN.store(depth, Ordering::Relaxed);
}
pub fn ingest_phase() -> IngestPhase {
    IngestPhase::from_u8(INGEST_PHASE.load(Ordering::Relaxed))
}
pub fn ingest_chan() -> u64 {
    INGEST_CHAN.load(Ordering::Relaxed)
}
```

- [ ] **Step 4: Add gauge fields + render the ingest line**

In `ferrolite-app/src/diag.rs`, add to `Gauges` (after `pub uploads_cap: usize,`):

```rust
    pub ingest_phase: IngestPhase,
    pub ingest_chan: u64,
```

`Gauges` derives `Default`; add `Default` for `IngestPhase` so the derive still works — add above the enum:

```rust
impl Default for IngestPhase {
    fn default() -> Self {
        IngestPhase::Idle
    }
}
```

Update `sample_gauges()` in the test module to set the two new fields (add `ingest_phase: IngestPhase::Idle,` and `ingest_chan: 0,`).

In `format_log`, append the ingest line to the format string only when a phase is active. Change the trailing `ingest active {ai}  done {idn}/{itot}` line to include a conditional ingest phase line. Simplest: build an `ingest_line` string before the `format!` and interpolate it:

```rust
    let ingest_line = if g.ingest_phase != IngestPhase::Idle {
        format!(
            "\n ingest  phase {ph} {idn}/{itot}  chan {chan}",
            ph = g.ingest_phase.label(),
            idn = g.ingest_done,
            itot = g.ingest_total,
            chan = g.ingest_chan,
        )
    } else {
        String::new()
    };
```

and append `{ingest_line}` at the very end of the `format_log` format string (after the `ingest active {ai}  done {idn}/{itot}` line), adding `ingest_line = ingest_line,` to the args. Do the same minimal addition in `format_overlay` (append the same `ingest_line` when active).

- [ ] **Step 5: Feed the gauge fields in app.rs**

In `ferrolite-app/src/app.rs`, in the `let gauges = crate::diag::Gauges { … }` construction, add:

```rust
                ingest_phase: crate::diag::ingest_phase(),
                ingest_chan: crate::diag::ingest_chan(),
```

- [ ] **Step 6: Publish phase transitions from ingest_job**

In `ferrolite-app/src/ingest.rs`, `ingest_job`, set the phase at each transition (gated — only meaningful when profiling, but the setter is cheap; guard with the existing `profile` to stay zero-cost when off):

- Before `scan_tree`: `if profile.is_some() { crate::diag::set_ingest_phase(crate::diag::IngestPhase::Scan); }`
- Before the Phase A block: `… IngestPhase::PhaseA`
- Before the `to_process` filter: `… IngestPhase::Filter`
- Before `to_process.par_iter()`: `… IngestPhase::Decode`
- Right before the final `IngestDone` send: `… IngestPhase::Idle` (clear the line when the job ends).

In the consumer `on_recv` and producer `on_send` sites (Task 2), also publish channel depth when profiling: after `p.on_send()` add `crate::diag::note_ingest_chan(p.chan_depth_max());` — actually publish the *current* inflight, not the max. Add a `chan_inflight()` reader to `IngestProfile` in Task 1? To avoid changing Task 1, publish the running max is misleading. Instead: in `on_send`/`on_recv`, have `ingest_job` call `crate::diag::note_ingest_chan(...)` is awkward from inside the closure. Simplest correct approach: add a `pub fn chan_inflight(&self) -> u64` getter to `IngestProfile` (add it in this task to `diag.rs`) and, after each `p.on_send()`/`p.on_recv()`, call `crate::diag::note_ingest_chan(p.chan_inflight())`.

Add to `IngestProfile` in `diag.rs`:

```rust
    pub fn chan_inflight(&self) -> u64 {
        self.chan_inflight.load(Ordering::Relaxed)
    }
```

- [ ] **Step 7: Run tests + gate**

Run: `cargo test -p ferrolite-app`
Expected: the two new format tests pass; all others pass.

Run: `cargo fmt -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add ferrolite-app/src/diag.rs ferrolite-app/src/ingest.rs ferrolite-app/src/app.rs
git commit -m "diag(app): live ingest phase/progress/channel line in log + overlay"
```

---

## Task 4: Workspace gate + author hand-off

**Files:** none (verification only).

- [ ] **Step 1: Full workspace gate**

Run:
```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all clean/green. (Windows `LNK1104` → re-run `cargo test --workspace` with `CARGO_TARGET_DIR=target-diag`.) Confirm zero `thumb_profile` / `FERROLITE_PROFILE_THUMBS` references remain: `git grep -n "thumb_profile\|FERROLITE_PROFILE_THUMBS" -- '*.rs'` returns nothing.

- [ ] **Step 2: HOLD for the author's instrumented run (CLAUDE.md)**

Do NOT merge/finish. Hand the author:

```powershell
$env:FERROLITE_DIAG = "1"; cargo run --release -p ferrolite-app
# open the big (~3320-image) folder; watch the overlay's live `ingest:` line;
# when it finishes, read the diag log file for the `[ingest-summary]` block.
```

What the summary answers: which phase eats the 5–10 min (scan / phaseA / filter / decode / consumer tail), whether decode is actually parallel (speedup vs cores), whether a few huge RAWs dominate (p95/max vs p50), and whether the DB/consumer is the tail (producer done@ vs consumer done@). Then read it together to decide the fix (a separate branch), and use superpowers:finishing-a-development-branch.

---

## Self-Review

**1. Spec coverage:**
- Remove `thumb_profile.rs` + `FERROLITE_PROFILE_THUMBS`, fold into diag → Task 2 Steps 5–7. ✓
- Per-job `IngestProfile` (created only when enabled, `Option<Arc>`) → Task 1 Step 3 + Task 2 Step 1. ✓
- Phase wall-clock (scan/phaseA/filter/decode/consumer tail) → Task 2 Steps 1–2, 4. ✓
- Decode parallel-speedup (Σ ÷ wall × cores) → Task 1 (format) + Task 2 Step 4 (`decode_sum_s`/`decode_par_s`/`cores`). ✓
- Per-file p50/p95/max + per-kind p50 → Task 1 (`percentile`, samples) + Task 2 Step 4. ✓
- Encode Σ/avg, upsert Σ/batches/avg → Task 1 + Task 2 Steps 3–4. ✓
- Producer/consumer lag → Task 2 Step 4 (`producer_done_s` vs `consumer_done_s`; format computes tail). ✓
- Channel depth (explicit inflight counter) → Task 1 (`on_send`/`on_recv`/`chan_depth_max`) + Task 2 producer/consumer. ✓
- One-shot `[ingest-summary]` + live `ingest:` line + `[thumb-blocking]` → Task 2 Step 4, Task 3, Task 1 Step 5. ✓
- Headless `thumbnail_blocking` preserved via diag; `bench_browse` builds; `ingest_tree` unaffected → Task 2 Steps 5, 8. ✓
- Zero-overhead-off (single `enabled()` resolve, `Option<Arc>` skip) → Task 2 Step 1. ✓
- Per-sec `thumb_profile::diag` removed → Task 2 Step 6. ✓

**2. Placeholder scan:** No TBD/TODO; every code step has complete code + exact commands.

**3. Type consistency:** `IngestProfile` methods (`record_decode(us,is_raw)`, `record_encode`, `record_upsert`, `on_send`, `on_recv`, `chan_inflight`, `*_samples`, atomic getters) match across Tasks 1–3. `IngestSummary` field names match `format_ingest_summary` and the Task-2 builder. `flush_batch`'s new `profile: Option<&Arc<crate::diag::IngestProfile>>` param matches its call sites (consumer). `IngestPhase` variants/labels + `Gauges.ingest_phase`/`ingest_chan` match `format_log`/`format_overlay`/app.rs. `percentile(&[u32], f64) -> u32` signature matches all call sites.

> Note: Task 1 is additive-only (new diag surface, `thumb_profile` still present) — a reviewer checks the new machinery in isolation. Task 2 does the behavior-changing rewire + removal. Task 3 adds the cosmetic live line. Clean seams.
