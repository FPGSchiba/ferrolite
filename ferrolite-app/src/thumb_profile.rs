//! Opt-in thumbnail profiling, enabled with the env var `FERROLITE_PROFILE_THUMBS`.
//!
//! Separates the per-file **disk read** cost from the **decode** and
//! **resize/encode** CPU costs so a slow-device bottleneck can be measured
//! against a real folder instead of guessed at. A cumulative summary line is
//! printed to stderr every `SUMMARY_EVERY` thumbnails. Zero overhead when off
//! (one cached bool check; nothing else runs).
//!
//! Usage: `FERROLITE_PROFILE_THUMBS=1 cargo run --release --bin ferrolite-app`,
//! open the slow folder, and watch stderr for `[thumb-profile]` lines.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// Bytes pre-read to force (and time) the disk IO a preview decode needs. The
/// embedded preview rawler reads sits within the first ~1 MB for the cameras
/// tested; 2 MiB gives headroom without reading pixel data.
const PROBE_READ_BYTES: usize = 2 << 20;
/// Emit a running summary every this many profiled thumbnails.
const SUMMARY_EVERY: u64 = 2;

static COUNT: AtomicU64 = AtomicU64::new(0);
static IO_US: AtomicU64 = AtomicU64::new(0);
static DECODE_US: AtomicU64 = AtomicU64::new(0);
static ENCODE_US: AtomicU64 = AtomicU64::new(0);
static WRITE_US: AtomicU64 = AtomicU64::new(0);
static READ_BYTES: AtomicU64 = AtomicU64::new(0);

// Ingest-producer instrumentation: per-file metadata-read and row-upsert cost.
static META_COUNT: AtomicU64 = AtomicU64::new(0);
static META_READ_US: AtomicU64 = AtomicU64::new(0);
static UPSERT_US: AtomicU64 = AtomicU64::new(0);

/// Record one file's metadata-read cost (producer side, parallel), microseconds.
pub fn record_meta(meta_read_us: u64) {
    if !enabled() {
        return;
    }
    META_COUNT.fetch_add(1, Ordering::Relaxed);
    META_READ_US.fetch_add(meta_read_us, Ordering::Relaxed);
}

/// Record one row's upsert cost (consumer side, serial under the writer lock),
/// microseconds — includes lock acquisition so consumer-side stalls are visible.
pub fn record_upsert(upsert_us: u64) {
    if !enabled() {
        return;
    }
    UPSERT_US.fetch_add(upsert_us, Ordering::Relaxed);
}

/// True iff `FERROLITE_PROFILE_THUMBS` is set (resolved once, then cached).
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FERROLITE_PROFILE_THUMBS").is_some())
}

/// Once-per-second pipeline diagnostic: indexing vs thumbnail completion rates
/// and job-pool occupancy. Reveals whether throughput is gated by job spawning
/// (indexing) or by workers being saturated with other (ingest) jobs. No-op when
/// profiling is off.
pub fn diag(indexed: u64, thumb_done: u64, thumb_total: u64, active: usize, pending: usize) {
    if !enabled() {
        return;
    }
    static START: OnceLock<Instant> = OnceLock::new();
    static LAST_MS: AtomicU64 = AtomicU64::new(0);
    static LAST_INDEXED: AtomicU64 = AtomicU64::new(0);
    static LAST_THUMB: AtomicU64 = AtomicU64::new(0);

    let start = START.get_or_init(Instant::now);
    let now_ms = start.elapsed().as_millis() as u64;
    let last = LAST_MS.load(Ordering::Relaxed);
    if now_ms.saturating_sub(last) < 1000 {
        return;
    }
    // Claim this tick (best-effort; a racing thread simply prints too).
    LAST_MS.store(now_ms, Ordering::Relaxed);
    let dt = now_ms.saturating_sub(last).max(1) as f64 / 1000.0;
    let d_idx = indexed.saturating_sub(LAST_INDEXED.swap(indexed, Ordering::Relaxed));
    let d_thumb = thumb_done.saturating_sub(LAST_THUMB.swap(thumb_done, Ordering::Relaxed));
    let mc = META_COUNT.load(Ordering::Relaxed).max(1);
    let avg_meta = META_READ_US.load(Ordering::Relaxed) as f64 / 1000.0 / mc as f64;
    let avg_upsert = UPSERT_US.load(Ordering::Relaxed) as f64 / 1000.0 / mc as f64;
    eprintln!(
        "[ingest-diag] indexed={indexed} (+{:.0}/s)  thumbs={thumb_done}/{thumb_total} (+{:.0}/s)  \
         jobs(active={active} pending={pending})  avg meta_read={avg_meta:.1}ms upsert={avg_upsert:.1}ms",
        d_idx as f64 / dt,
        d_thumb as f64 / dt,
    );
}

/// Force + time the cold disk read for `path` (the bytes a preview decode pages
/// in). Also warms the OS cache so the decode timed next reflects CPU only.
/// Returns the read duration in microseconds.
pub fn measure_read(path: &Path) -> u64 {
    use std::io::Read;
    let t = Instant::now();
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut buf = vec![0u8; PROBE_READ_BYTES];
        if let Ok(n) = f.read(&mut buf) {
            READ_BYTES.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
    t.elapsed().as_micros() as u64
}

/// Record one profiled thumbnail's phase timings (microseconds) and print a
/// cumulative summary every `SUMMARY_EVERY` files. `write_us` covers acquiring
/// the shared writer lock **and** the SQLite `put_thumbnail` commit, so lock
/// contention and DB-write cost are visible separately from disk read + CPU.
pub fn record(io_us: u64, decode_us: u64, encode_us: u64, write_us: u64) {
    let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let io = IO_US.fetch_add(io_us, Ordering::Relaxed) + io_us;
    let dec = DECODE_US.fetch_add(decode_us, Ordering::Relaxed) + decode_us;
    let enc = ENCODE_US.fetch_add(encode_us, Ordering::Relaxed) + encode_us;
    let wr = WRITE_US.fetch_add(write_us, Ordering::Relaxed) + write_us;
    if !n.is_multiple_of(SUMMARY_EVERY) {
        return;
    }
    let bytes = READ_BYTES.load(Ordering::Relaxed);
    let mbps = if io > 0 {
        (bytes as f64) / (io as f64) // bytes/µs == MB/s
    } else {
        0.0
    };
    let nf = n as f64;
    eprintln!(
        "[thumb-profile] n={n}  avg/file: io={:.1}ms decode={:.1}ms encode={:.1}ms write={:.1}ms \
         | read {:.0}MB @ {:.1}MB/s",
        io as f64 / 1000.0 / nf,
        dec as f64 / 1000.0 / nf,
        enc as f64 / 1000.0 / nf,
        wr as f64 / 1000.0 / nf,
        bytes as f64 / 1e6,
        mbps,
    );
}
