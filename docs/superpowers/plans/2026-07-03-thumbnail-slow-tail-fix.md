# Thumbnail Slow-Tail Fix — incremental ingest source read

> **For agentic workers:** implement task-by-task; steps are checkboxes.

**Goal:** Eliminate the ~5 s/file over-read that dominates ingest (80% of decode-sum) by replacing `with_ingest_source`'s eager full-file mmap fallback with an incremental, bounded, single-open sequential read.

**Confirmed root cause (two instrumented runs):** the 1 MiB `INGEST_PREFIX` misses for ~12% of files (front-stored embedded JPEG crosses 1 MiB); the miss falls back to `rawler::RawSource::new(path)` which — via `MmapOptions::populate()` + `madvise(WillNeed|Sequential)` — reads the ENTIRE 24–50 MB file off a slow SD card just to `subview` a ≤2 MB preview. `acquire` (that call) = 1974.9 s of the 2105 s slow-tail; slow-set == fallback-set exactly (390 files).

**Fix:** open the file once; grow an in-memory buffer through caps `[1 MiB, 8 MiB]`, then to EOF; retry the decode (`RawSource::new_from_slice`) at each cap; return as soon as it succeeds. No regression to the 88% (still one 1 MiB read); the 12% become one ≤8 MiB read instead of a whole-file read; correctness preserved by the final full-read tier.

**Tech Stack:** Rust; `rawler` 0.7.2 (`RawSource::new_from_slice` builds an in-memory `Memory` source that `subview` slices identically to the mmap one).

## Global Constraints

- Never block the UI/update thread; this code runs only on `ferrolite-jobs` ingest workers. Behavior on the successful path must feed rawler the same bytes it sees today (correctness identical). (CLAUDE.md)
- `ferrolite-decode` stays diagnostics-free: no diag dep, no env reads; it only *reports* via `SourceProbe`/`PreviewInfo`, timing only when `measure=true` (no `Instant` when false). (established)
- Diag zero-overhead when `FERROLITE_DIAG` unset. ASCII-only diagnostic output. `cargo fmt`/`clippy -D warnings` clean. No git attribution trailers.
- Final gate: `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green, then HOLD for the author's instrumented re-test vs the 252 s / acquire-1975 s baseline.

## File Structure

- `ferrolite-decode/src/source.rs` — MODIFY: rewrite `with_ingest_source` (incremental read); `SourceKind` → `{Prefix, Grown, Full}`; `SourceProbe` gains `bytes: Option<u64>`; add read-tier consts + a unit test.
- `ferrolite-decode/src/lib.rs` — MODIFY: `PreviewInfo.source_bytes: Option<u64>`; thread `probe.bytes`. `SourceKind` re-export unchanged.
- `ferrolite-app/src/diag.rs` — MODIFY: `source_kind_label` (3 variants); `SlowSample.source_bytes`; per-file line + aggregate show tier + MB; `IngestProfile` counts prefix/grown/full; `format_source_split` 3-way; tests.
- `ferrolite-app/src/ingest.rs` — MODIFY: populate `source_bytes`; record 3-way tier; emit updated split.

---

### Task 1: `ferrolite-decode` — incremental single-open ingest read

**Files:** `ferrolite-decode/src/source.rs`, `ferrolite-decode/src/lib.rs`, plus compile-follow in `ferrolite-app` (Task 2 does the real diag work).

**Interfaces produced:**
- `pub enum SourceKind { Prefix, Grown, Full }`
- `pub struct SourceProbe { pub kind: SourceKind, pub acquire: Option<Duration>, pub bytes: Option<u64> }`
- `with_ingest_source(path, measure, f) -> Result<(T, SourceProbe), DecodeError>` — unchanged signature shape, new read strategy.
- `PreviewInfo.source_bytes: Option<u64>` (bytes read to satisfy the decode).

- [ ] **Step 1: Rewrite `source.rs`.** Replace the `SourceKind`/`SourceProbe`/`read_prefix`/`with_ingest_source` region with:

```rust
/// Byte caps at which we pause the sequential read and retry the decode. 1 MiB
/// covers ~88% of files (front-stored embedded preview); 8 MiB covers busier
/// scenes whose preview crosses 1 MiB; past the last cap we read to EOF so any
/// file still decodes correctly. Growing an in-memory buffer (vs rawler's
/// `RawSource::new`, which mmap-populates the WHOLE file) is the fix: we read
/// only as far as the decode actually needs.
const INGEST_READ_CAPS: [usize; 2] = [1 << 20, 8 << 20]; // 1 MiB, 8 MiB

/// Which read tier satisfied the decode. Diagnostic-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Satisfied by the first (1 MiB) read — the fast path.
    Prefix,
    /// Needed a larger bounded read but not the whole file.
    Grown,
    /// Needed the entire file (read to EOF).
    Full,
}

/// Diagnostic probe of how a file's bytes were obtained. `acquire` is the total
/// time spent reading bytes for the successful attempt; `bytes` is how many were
/// read. Both `Some` only when `measure = true`.
#[derive(Debug, Clone, Copy)]
pub struct SourceProbe {
    pub kind: SourceKind,
    pub acquire: Option<Duration>,
    pub bytes: Option<u64>,
}

/// Read `path` through the decode `f`, growing an in-memory buffer only as far
/// as the decode needs: try after 1 MiB, then 8 MiB, then the whole file. The
/// file is opened once and read sequentially (bytes are appended, never
/// re-read). `f` may be called up to three times, so it must be side-effect-free
/// on failure (all our uses are pure reads). Replaces the previous
/// `RawSource::new` mmap fallback, which eagerly read the entire file.
pub(crate) fn with_ingest_source<T>(
    path: &Path,
    measure: bool,
    f: impl Fn(&RawSource) -> Result<T, DecodeError>,
) -> Result<(T, SourceProbe), DecodeError> {
    let mut file = File::open(path).map_err(DecodeError::Io)?;
    let mut buf: Vec<u8> = Vec::new();
    let mut acquire = Duration::ZERO;
    let mut last_err: Option<DecodeError> = None;
    let mut at_eof = false;

    // Cap tiers, then a sentinel `usize::MAX` meaning "read to EOF".
    let targets = INGEST_READ_CAPS
        .iter()
        .copied()
        .chain(std::iter::once(usize::MAX));

    for (i, target) in targets.enumerate() {
        if at_eof && i > 0 {
            break; // already read everything on a previous tier
        }
        // Read forward until `buf` reaches `target` bytes or EOF.
        let t = measure.then(Instant::now);
        at_eof = read_up_to(&mut file, &mut buf, target).map_err(DecodeError::Io)?;
        if let Some(t) = t {
            acquire += t.elapsed();
        }

        match f(&RawSource::new_from_slice(&buf)) {
            Ok(v) => {
                let kind = if i == 0 {
                    SourceKind::Prefix
                } else if at_eof {
                    SourceKind::Full
                } else {
                    SourceKind::Grown
                };
                return Ok((
                    v,
                    SourceProbe {
                        kind,
                        acquire: measure.then_some(acquire),
                        bytes: measure.then_some(buf.len() as u64),
                    },
                ));
            }
            Err(e) => last_err = Some(e),
        }

        if at_eof {
            break; // nothing more to read; the decode genuinely failed
        }
    }

    Err(last_err.unwrap_or_else(|| DecodeError::NoPreview(path.to_path_buf())))
}

/// Append bytes from `file` to `buf` until `buf.len()` reaches `target` (or
/// `usize::MAX` = until EOF). Returns `true` if EOF was reached. Sequential,
/// no re-reads: each call continues from the file's current position.
fn read_up_to(file: &mut File, buf: &mut Vec<u8>, target: usize) -> std::io::Result<bool> {
    const CHUNK: usize = 256 * 1024;
    loop {
        if target != usize::MAX && buf.len() >= target {
            return Ok(false);
        }
        let want = if target == usize::MAX {
            CHUNK
        } else {
            CHUNK.min(target - buf.len())
        };
        let start = buf.len();
        buf.resize(start + want, 0);
        let n = file.read(&mut buf[start..])?;
        buf.truncate(start + n);
        if n == 0 {
            return Ok(true); // EOF
        }
    }
}
```

Keep the existing imports; ensure `use std::fs::File; use std::io::Read; use std::time::{Duration, Instant};` are present. Confirm `DecodeError::Io(std::io::Error)` exists — if the variant is named differently, use the existing IO-error constructor (check `error.rs`); do NOT invent one.

- [ ] **Step 2: `lib.rs` — add `source_bytes` to `PreviewInfo`** and set it from the probe. In the `PreviewInfo` struct add `pub source_bytes: Option<u64>,` (next to `source_acquire`). In the RAW arm assembly set `source_bytes: probe.bytes,`. In the `Standard` arm set `source_bytes: None,`.

- [ ] **Step 3: `lib.rs`/`preview.rs` callers** — `read_metadata_raw` and `decode_preview_raw` already `.map(|(v, _probe)| v)`; unchanged (they pass `measure=false`).

- [ ] **Step 4: Unit test in `source.rs`** (no RAW fixture needed — exercises the tier logic with a synthetic file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(bytes: &[u8]) -> std::path::PathBuf {
        // Unique name via the buffer length + a nanos-free counter is unnecessary;
        // use the process temp dir with the byte-len in the name.
        let mut p = std::env::temp_dir();
        p.push(format!("ferrolite-src-test-{}.bin", bytes.len()));
        let mut fandtrunc = File::create(&p).unwrap();
        fandtrunc.write_all(bytes).unwrap();
        p
    }

    #[test]
    fn satisfies_at_first_tier_when_marker_in_prefix() {
        // 3 MiB file with a marker byte at offset 10 (inside 1 MiB).
        let mut data = vec![0u8; 3 << 20];
        data[10] = 0xAB;
        let path = temp_file(&data);
        // f succeeds iff the marker byte is present in the buffer.
        let (v, probe) = with_ingest_source(&path, true, |src| {
            if src.buf().get(10) == Some(&0xAB) { Ok(42) }
            else { Err(DecodeError::NoPreview(std::path::PathBuf::new())) }
        })
        .unwrap();
        assert_eq!(v, 42);
        assert_eq!(probe.kind, SourceKind::Prefix);
        assert!(probe.bytes.unwrap() <= (1 << 20) + 256 * 1024);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn grows_to_second_tier_when_marker_past_prefix() {
        // 12 MiB file with the marker at 5 MiB (past 1 MiB, within 8 MiB).
        let mut data = vec![0u8; 12 << 20];
        data[5 << 20] = 0xCD;
        let path = temp_file(&data);
        let (v, probe) = with_ingest_source(&path, true, |src| {
            if src.buf().get(5 << 20) == Some(&0xCD) { Ok(7) }
            else { Err(DecodeError::NoPreview(std::path::PathBuf::new())) }
        })
        .unwrap();
        assert_eq!(v, 7);
        assert_eq!(probe.kind, SourceKind::Grown);
        assert!(probe.bytes.unwrap() <= (8 << 20) + 256 * 1024);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reads_to_eof_when_marker_deep() {
        // 10 MiB file, marker at 9 MiB (past 8 MiB) → Full.
        let mut data = vec![0u8; 10 << 20];
        data[9 << 20] = 0xEF;
        let path = temp_file(&data);
        let (v, probe) = with_ingest_source(&path, true, |src| {
            if src.buf().get(9 << 20) == Some(&0xEF) { Ok(9) }
            else { Err(DecodeError::NoPreview(std::path::PathBuf::new())) }
        })
        .unwrap();
        assert_eq!(v, 9);
        assert_eq!(probe.kind, SourceKind::Full);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn measure_false_records_no_timings() {
        let data = vec![0u8; 2 << 20];
        let path = temp_file(&data);
        let (_v, probe) = with_ingest_source(&path, false, |_src| Ok(1)).unwrap();
        assert!(probe.acquire.is_none() && probe.bytes.is_none());
        let _ = std::fs::remove_file(&path);
    }
}
```

(If `source.rs` already has a `#[cfg(test)] mod tests`, add these into it. Reuse a unique temp-name scheme; the byte-len suffix keeps the four tests from colliding.)

- [ ] **Step 5:** Build the decode crate + compile-follow the app’s use of `SourceKind`/`source_bytes` minimally so the workspace builds (the real diag wiring is Task 2). Run:

```
cargo test -p ferrolite-decode
cargo build --workspace
```
Expected: decode tests pass. The workspace build will fail in `ferrolite-app` until Task 2 (SourceKind lost its `Fallback` variant; `source_bytes` is new) — that is expected; do NOT bodge diag here. If you want the build green at this commit, apply only the mechanical renames Task 2 Step 1–2 specify. Otherwise commit Task 1 (decode-only) and proceed immediately to Task 2.

- [ ] **Step 6: Commit.**
```
git add ferrolite-decode/src/source.rs ferrolite-decode/src/lib.rs
git commit -m "perf(decode): incremental single-open ingest read (drop full-file mmap over-read)"
```

---

### Task 2: `ferrolite-app` diag — 3-way tier reporting + bytes

**Files:** `ferrolite-app/src/diag.rs`, `ferrolite-app/src/ingest.rs`

**Interfaces consumed:** `ferrolite_decode::SourceKind {Prefix, Grown, Full}`; `PreviewInfo.source_bytes`.

- [ ] **Step 1: `diag.rs` — `source_kind_label` 3 variants.**
```rust
pub fn source_kind_label(k: Option<ferrolite_decode::SourceKind>) -> &'static str {
    match k {
        Some(ferrolite_decode::SourceKind::Prefix) => "prefix",
        Some(ferrolite_decode::SourceKind::Grown) => "grown",
        Some(ferrolite_decode::SourceKind::Full) => "full",
        None => "n/a",
    }
}
```

- [ ] **Step 2: `diag.rs` — `SlowSample` gains `source_bytes: u64`** (0 when unknown). Show it in `format_slow_line` as MB after the tier tag, e.g. `[grown 5.0MB]`:
```rust
// in format_slow_line, replace the tier tag:
"[ingest-slow] {dec:.0}ms [{tier} {mb:.1}MB] (acquire {acq:.0} / ...",
tier = source_kind_label(s.source_kind),
mb = s.source_bytes as f64 / 1_048_576.0,
```

- [ ] **Step 3: `diag.rs` — `IngestProfile` counts three tiers.** Replace `prefix_hits`/`fallbacks` with `prefix_hits`/`grown`/`full` (AtomicU64), update `record_source` to match on the three variants, add getters. Update `format_source_split(prefix, grown, full)` to:
```rust
pub fn format_source_split(prefix: u64, grown: u64, full: u64) -> String {
    let total = prefix + grown + full;
    let non_prefix = grown + full;
    let pct = if total > 0 { 100.0 * non_prefix as f64 / total as f64 } else { 0.0 };
    format!(
        "[ingest-source] RAW byte-source: prefix {prefix} | grown {grown} | full {full} \
         ({pct:.1}% needed >1MiB of {total})"
    )
}
```

- [ ] **Step 4: `diag.rs` — `format_slow_aggregate` by-path line** → `prefix P | grown G | full F` using the three `count_kind`s; keep the stage-sum rollup.

- [ ] **Step 5: update the diag tests** for the new `SlowSample` field (`source_bytes`), the 3-arg `format_source_split` (assert `prefix`/`grown`/`full` substrings), the `[grown` tag, and `record_source`/`prefix_hits`/`grown`/`full` counters. The `slow_sample` helper: default `source_kind: Some(SourceKind::Grown)`, `source_bytes: 8<<20`.

- [ ] **Step 6: `ingest.rs`** — populate `source_bytes: info.source_bytes.unwrap_or(0)` in the `SlowSample`; `record_source(kind)` already called for `Some(kind)`; update the aggregate emit to `emit_source_split(p.prefix_hits(), p.grown(), p.full())`.

- [ ] **Step 7: Gate.**
```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all green.

- [ ] **Step 8: Commit.**
```
git add ferrolite-app/src/diag.rs ferrolite-app/src/ingest.rs
git commit -m "diag(app): 3-way source-tier reporting (prefix/grown/full) + bytes"
```

---

### Task 3: Gate + HOLD

- [ ] Full gate (fmt/clippy/test) green.
- [ ] HOLD for the author's instrumented re-run (`FERROLITE_DIAG=1`). Expected: `[ingest-source]` shows most former fallbacks now `grown` at a few MB; `acquire` per slow file drops from ~5 s to sub-second; total ingest 252 s → target ~60–90 s; `full` count small. If many land in `full`, the tier log tells us the preview sits deep and we consider the offset-directed refinement.

## Self-Review

- Spec coverage: incremental read (Task 1) + self-measuring tiers (Task 1 probe + Task 2 reporting) + gate/hold (Task 3). ✔
- Correctness: final EOF tier reproduces today's whole-file view, so any file that decodes today still decodes. Successful-path bytes fed to rawler are a prefix/superset containing exactly what `subview` needs — identical decode result. ✔
- No placeholders; `DecodeError::Io` usage flagged to verify against `error.rs`. ✔
- Zero-overhead-off: reads happen regardless (they must — it's the real work), but `Instant`/`bytes` only recorded when `measure`. No diag dep in decode. ✔
- Type consistency: `SourceKind {Prefix,Grown,Full}` used identically in decode, `source_kind_label`, counters, `format_source_split`. ✔
