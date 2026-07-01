//! Cheap RAW access for ingest: read a *sequential* prefix of the file and hand
//! rawler an in-memory `RawSource`, instead of letting it memory-map and touch
//! the file through scattered page faults.
//!
//! Why this matters: rawler's `raw_metadata` / `raw_image(dummy)` / preview
//! extraction only need the metadata IFDs and the embedded preview, which sit in
//! the first ~1 MB of the file for the cameras we target. Under mmap, though,
//! rawler pages those bytes in as ~hundreds of scattered 4 KB faults — each a
//! disk seek. On a high-latency disk that is seconds per file, and with many
//! concurrent readers it collapses into seek-thrashing (measured: ~15 s/file).
//! Reading one sequential prefix up front turns that into a single fast read.

use crate::error::{rawler as rawler_err, DecodeError};
use rawler::rawsource::RawSource;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Sequential prefix read for ingest. Covers the metadata IFDs and the embedded
/// preview (both live in the first ~1 MB for the RAWs we target); the mmap
/// fallback in `with_ingest_source` handles the rare file that needs more.
const INGEST_PREFIX: usize = 1 << 20; // 1 MiB

/// Read up to `INGEST_PREFIX` bytes from the start of `path` in one sequential read.
fn read_prefix(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; INGEST_PREFIX];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

/// Run `f` against a `RawSource` built from the file's sequential prefix; if that
/// fails (the decoder needed more than the prefix, or the prefix read failed),
/// retry against the full memory-mapped file so correctness is never sacrificed.
///
/// `f` may be called twice, so it must be side-effect-free on failure (all our
/// uses are pure reads).
pub(crate) fn with_ingest_source<T>(
    path: &Path,
    f: impl Fn(&RawSource) -> Result<T, DecodeError>,
) -> Result<T, DecodeError> {
    if let Ok(buf) = read_prefix(path) {
        let src = RawSource::new_from_slice(&buf);
        if let Ok(v) = f(&src) {
            return Ok(v);
        }
    }
    // Fallback: full mmap source (correct for the rare file whose metadata or
    // preview lives past the prefix).
    let src = RawSource::new(path).map_err(rawler_err)?;
    f(&src)
}
