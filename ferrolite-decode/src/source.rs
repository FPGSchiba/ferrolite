//! Cheap RAW access for ingest: read the file *sequentially* into an in-memory
//! buffer, growing it only as far as the decode actually needs, and hand rawler
//! an in-memory `RawSource` over that buffer.
//!
//! Why this matters: rawler's `raw_metadata` / `raw_image(dummy)` / preview
//! extraction only need the metadata IFDs and the embedded preview, which sit in
//! the first few MB of the file for the cameras we target. rawler's own
//! `RawSource::new` mmaps the file with `populate()` + `madvise(WILLNEED)`, which
//! reads the *entire* 24–50 MB file into memory at map time — even though we only
//! slice a <=2 MB preview out of it. On a slow disk (e.g. an SD card) that is
//! ~5 s/file and, measured, ~80% of total ingest time. Instead we read a 1 MiB
//! prefix (covers ~88% of files), and on a miss grow to 8 MiB, then to EOF —
//! reading only as much as the decode needs and never the whole file unless a
//! file's preview genuinely lives that deep.

use crate::error::DecodeError;
use rawler::rawsource::RawSource;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

/// Byte caps at which we pause the sequential read and retry the decode. 1 MiB
/// covers ~88% of files (front-stored embedded preview); the finer 2/4/8 MiB
/// steps let a file whose preview crosses 1 MiB stop as soon as enough is read,
/// instead of always pulling 8 MiB — on a slow SD card every extra MiB costs.
/// Past the last cap we read to EOF so any file still decodes correctly.
const INGEST_READ_CAPS: [usize; 4] = [1 << 20, 2 << 20, 4 << 20, 8 << 20];

/// Chunk size for the sequential read. Sized so the 1 MiB prefix is a single
/// read syscall (a smaller chunk measurably slowed the fast path on the SD card).
const READ_CHUNK: usize = 1 << 20;

/// Which read tier satisfied the decode. Diagnostic-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Satisfied by the first (1 MiB) read — the fast path.
    Prefix,
    /// Offset-parsing found the exact preview span; the read stopped there.
    Directed,
    /// Needed a larger bounded read (past 1 MiB) but not the whole file.
    Grown,
    /// Needed the entire file (read to EOF).
    Full,
}

/// Diagnostic probe of how a file's bytes were obtained. `acquire` is the total
/// time spent reading bytes for the successful attempt; `bytes` is how many were
/// read. Both `Some` only when the caller passed `measure = true` (zero `Instant`
/// cost when false).
#[derive(Debug, Clone, Copy)]
pub struct SourceProbe {
    pub kind: SourceKind,
    pub acquire: Option<Duration>,
    pub bytes: Option<u64>,
}

/// Append bytes from `file` to `buf` until `buf.len()` reaches `target` (or, when
/// `target == usize::MAX`, until EOF). Returns `true` if EOF was reached.
/// Sequential and cumulative: each call continues from the file's current
/// position, so bytes are never re-read across tiers.
fn read_up_to(file: &mut File, buf: &mut Vec<u8>, target: usize) -> std::io::Result<bool> {
    loop {
        if target != usize::MAX && buf.len() >= target {
            return Ok(false);
        }
        let want = if target == usize::MAX {
            READ_CHUNK
        } else {
            READ_CHUNK.min(target - buf.len())
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

/// Run the decode `f` against an in-memory `RawSource`, growing the buffer only
/// as far as the decode needs: try after 1 MiB, then 8 MiB, then the whole file.
/// The file is opened once and read sequentially (bytes are appended, never
/// re-read). Returns the decode result plus a [`SourceProbe`] reporting which
/// tier satisfied it (and, when `measure`, the read time + bytes read).
///
/// `f` may be called up to three times, so it must be side-effect-free on
/// failure (all our uses are pure reads).
pub(crate) fn with_ingest_source<T>(
    path: &Path,
    measure: bool,
    f: impl Fn(&RawSource) -> Result<T, DecodeError>,
) -> Result<(T, SourceProbe), DecodeError> {
    let mut file = File::open(path)?;
    let mut buf: Vec<u8> = Vec::new();
    let mut acquire = Duration::ZERO;

    // First (1 MiB) read, tried directly so a prefix-parse failure can attempt
    // one offset-directed read before falling into the tier loop.
    let t = measure.then(Instant::now);
    read_up_to(&mut file, &mut buf, INGEST_READ_CAPS[0])?;
    if let Some(t) = t {
        acquire += t.elapsed();
    }

    let mut last_err = match f(&RawSource::new_from_slice(&buf)) {
        Ok(v) => {
            return Ok((
                v,
                SourceProbe {
                    kind: SourceKind::Prefix,
                    acquire: measure.then_some(acquire),
                    bytes: measure.then_some(buf.len() as u64),
                },
            ));
        }
        Err(e) => e,
    };

    // The preview wasn't in the prefix. If offset-parsing can find the exact
    // preview span from the TIFF IFDs already in `buf`, do ONE directed read
    // straight to that span instead of growing through the 2/4/8 MiB tiers.
    // `read_up_to` naturally clamps at EOF if the file is shorter than the
    // parsed span, so a bogus/oversized offset just falls through below.
    if let Some(span) = crate::ifd::preview_span_end(&buf) {
        if span.end > buf.len() as u64 {
            let target = usize::try_from(span.end).unwrap_or(usize::MAX);
            let t = measure.then(Instant::now);
            read_up_to(&mut file, &mut buf, target)?;
            if let Some(t) = t {
                acquire += t.elapsed();
            }
            match f(&RawSource::new_from_slice(&buf)) {
                Ok(v) => {
                    return Ok((
                        v,
                        SourceProbe {
                            kind: SourceKind::Directed,
                            acquire: measure.then_some(acquire),
                            bytes: measure.then_some(buf.len() as u64),
                        },
                    ));
                }
                Err(e) => last_err = e,
            }
        }
    }

    // Fallback: existing tiered incremental read, starting from the 2 MiB cap
    // (the 1 MiB prefix was already read and, on a directed-read miss, so was
    // the directed span — both are reused via `buf`, never re-read).
    let targets = INGEST_READ_CAPS[1..]
        .iter()
        .copied()
        .chain(std::iter::once(usize::MAX));

    for target in targets {
        let t = measure.then(Instant::now);
        let at_eof = read_up_to(&mut file, &mut buf, target)?;
        if let Some(t) = t {
            acquire += t.elapsed();
        }

        match f(&RawSource::new_from_slice(&buf)) {
            Ok(v) => {
                let kind = if at_eof {
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
            Err(e) => last_err = e,
        }

        // Nothing more to read: the decode genuinely failed on the whole file.
        if at_eof {
            break;
        }
    }

    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write `bytes` to a uniquely-named temp file (unique by byte-len + marker
    /// offset, which the callers vary) and return its path.
    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("ferrolite-src-test-{name}.bin"));
        let mut f = File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        p
    }

    /// `f` that succeeds iff the marker byte `0xAB` is present at `at`.
    fn needs_byte(at: usize) -> impl Fn(&RawSource) -> Result<u32, DecodeError> {
        move |src| {
            if src.buf().get(at) == Some(&0xAB) {
                Ok(42)
            } else {
                Err(DecodeError::NoPreview(std::path::PathBuf::new()))
            }
        }
    }

    #[test]
    fn satisfies_at_first_tier_when_marker_in_prefix() {
        let mut data = vec![0u8; 3 << 20];
        data[10] = 0xAB; // inside the 1 MiB prefix
        let path = temp_file("prefix", &data);
        let (v, probe) = with_ingest_source(&path, true, needs_byte(10)).unwrap();
        assert_eq!(v, 42);
        assert_eq!(probe.kind, SourceKind::Prefix);
        assert!(probe.bytes.unwrap() <= (1 << 20) + READ_CHUNK as u64);
        let _ = std::fs::remove_file(&path);
    }

    /// Build a minimal LE TIFF header (mirrors `ifd::tests::tiny_tiff`) whose
    /// JPEGInterchangeFormat offset+length point at the embedded preview span.
    fn tiny_tiff(preview_off: u32, preview_len: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"II"); // little-endian
        b.extend_from_slice(&42u16.to_le_bytes()); // magic
        b.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset = 8
                                                  // IFD0 at offset 8: entry count = 2
        b.extend_from_slice(&2u16.to_le_bytes());
        let entry = |tag: u16, val: u32| {
            let mut e = Vec::new();
            e.extend_from_slice(&tag.to_le_bytes());
            e.extend_from_slice(&4u16.to_le_bytes()); // LONG
            e.extend_from_slice(&1u32.to_le_bytes()); // count
            e.extend_from_slice(&val.to_le_bytes()); // value/offset
            e
        };
        b.extend_from_slice(&entry(513, preview_off)); // JPEGInterchangeFormat
        b.extend_from_slice(&entry(514, preview_len)); // JPEGInterchangeFormatLength
        b.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0
        b
    }

    #[test]
    fn directed_read_satisfies_preview_just_past_prefix_in_one_read() {
        // Marker sits just past the 1 MiB prefix; the TIFF header at the front
        // declares a JPEGInterchangeFormat span that exactly covers it, so
        // offset-parsing should drive a single directed read straight there
        // instead of growing through the 2/4/8 MiB tiers.
        let marker_at = (1 << 20) + 4096; // just past 1 MiB
        let preview_len = 8u32;
        let mut data = vec![0u8; marker_at + preview_len as usize + 16];
        let header = tiny_tiff(marker_at as u32, preview_len);
        data[..header.len()].copy_from_slice(&header);
        data[marker_at] = 0xAB;
        let path = temp_file("directed", &data);

        let (v, probe) = with_ingest_source(&path, true, needs_byte(marker_at)).unwrap();
        assert_eq!(v, 42);
        assert_eq!(probe.kind, SourceKind::Directed);
        // Satisfied by the exact span end, not a full 8 MiB-tier read.
        let span_end = marker_at as u64 + preview_len as u64;
        let bytes = probe.bytes.unwrap();
        assert!(
            bytes < (2 << 20) as u64,
            "expected a directed read well under the 2 MiB tier, got {bytes}"
        );
        assert!(
            bytes >= span_end,
            "directed read must cover at least the preview span end"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn grows_to_second_tier_when_marker_past_prefix() {
        let mut data = vec![0u8; 12 << 20];
        data[5 << 20] = 0xAB; // past 1 MiB, within 8 MiB
        let path = temp_file("grown", &data);
        let (v, probe) = with_ingest_source(&path, true, needs_byte(5 << 20)).unwrap();
        assert_eq!(v, 42);
        assert_eq!(probe.kind, SourceKind::Grown);
        assert!(probe.bytes.unwrap() <= (8 << 20) + READ_CHUNK as u64);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reads_to_eof_when_marker_deep() {
        let mut data = vec![0u8; 10 << 20];
        data[9 << 20] = 0xAB; // past 8 MiB → needs the whole file
        let path = temp_file("full", &data);
        let (v, probe) = with_ingest_source(&path, true, needs_byte(9 << 20)).unwrap();
        assert_eq!(v, 42);
        assert_eq!(probe.kind, SourceKind::Full);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn errors_when_decode_never_succeeds() {
        let data = vec![0u8; 2 << 20]; // no marker anywhere
        let path = temp_file("nomarker", &data);
        let r = with_ingest_source(&path, false, needs_byte(1 << 20));
        assert!(r.is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn measure_false_records_no_timings() {
        let data = vec![0u8; 2 << 20];
        let path = temp_file("measure-off", &data);
        let (_v, probe) = with_ingest_source(&path, false, |_src| Ok(1u32)).unwrap();
        assert!(probe.acquire.is_none() && probe.bytes.is_none());
        let _ = std::fs::remove_file(&path);
    }
}
