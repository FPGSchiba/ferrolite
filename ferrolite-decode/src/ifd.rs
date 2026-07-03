//! Minimal, pure, panic-free TIFF/EXIF IFD walker used to compute the byte
//! span that must be resident (read from disk) before `rawler` can extract a
//! RAW file's embedded preview and read its metadata. This is NOT a general
//! TIFF parser: it only looks for the tags needed to bound that span
//! (`JPEGInterchangeFormat`/`Length` and `StripOffsets`/`StripByteCounts`),
//! walking IFD0 and, if present, one level of `SubIFDs`.
//!
//! Every offset read from the file is validated against `prefix.len()` before
//! use; the function never panics or indexes out of bounds, even on
//! truncated, garbage, or adversarial input — it returns `None` instead.

const TIFF_MAGIC: u16 = 42;
const IFD_ENTRY_SIZE: u64 = 12;

const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;
const TAG_SUB_IFDS: u16 = 330;
const TAG_JPEG_INTERCHANGE_FORMAT: u16 = 513;
const TAG_JPEG_INTERCHANGE_FORMAT_LENGTH: u16 = 514;

const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;

/// Minimal byte range that must be resident for rawler to extract the embedded
/// preview + read metadata, parsed from a TIFF prefix. `end` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewSpan {
    pub end: u64,
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn read_u16(self, buf: &[u8], offset: u64) -> Option<u16> {
        let start = usize::try_from(offset).ok()?;
        let bytes = buf.get(start..start.checked_add(2)?)?;
        Some(match self {
            Endian::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
            Endian::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
        })
    }

    fn read_u32(self, buf: &[u8], offset: u64) -> Option<u32> {
        let start = usize::try_from(offset).ok()?;
        let bytes = buf.get(start..start.checked_add(4)?)?;
        Some(match self {
            Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        })
    }
}

/// One decoded IFD entry: tag, field type, count, and the raw 4-byte
/// value/offset slot (still in file byte order; interpretation depends on
/// `type_` and `count`).
struct RawEntry {
    tag: u16,
    type_: u16,
    count: u32,
    value_offset: u64,
}

/// Parse a TIFF/EXIF header at the start of `prefix`; return the max end offset
/// (`offset + length`) among the embedded-preview strips (JPEGInterchangeFormat
/// 513/514 and StripOffsets 273 / StripByteCounts 279) found in IFD0 + SubIFDs.
/// Returns None if `prefix` is not a parseable TIFF or no preview pointer found.
pub fn preview_span_end(prefix: &[u8]) -> Option<PreviewSpan> {
    let endian = detect_endian(prefix)?;
    let magic = endian.read_u16(prefix, 2)?;
    if magic != TIFF_MAGIC {
        return None;
    }
    let ifd0_offset = u64::from(endian.read_u32(prefix, 4)?);

    let mut max_end: Option<u64> = None;
    accumulate_ifd_span(prefix, endian, ifd0_offset, &mut max_end, true);

    max_end.map(|end| PreviewSpan { end })
}

fn detect_endian(prefix: &[u8]) -> Option<Endian> {
    match prefix.get(0..2)? {
        b"II" => Some(Endian::Little),
        b"MM" => Some(Endian::Big),
        _ => None,
    }
}

/// Walk one IFD at `ifd_offset`, folding any preview-strip pairs it directly
/// contains into `max_end`, and — if `follow_sub_ifds` is set — recursing one
/// level into any `SubIFDs` (330) entries found. All offsets are bounds
/// checked against `prefix.len()`; any failure silently contributes nothing
/// (the overall result is `None` only if no valid pair was found anywhere).
fn accumulate_ifd_span(
    prefix: &[u8],
    endian: Endian,
    ifd_offset: u64,
    max_end: &mut Option<u64>,
    follow_sub_ifds: bool,
) {
    let Some(entries) = read_ifd_entries(prefix, endian, ifd_offset) else {
        return;
    };

    let jpeg_off = find_entry(&entries, TAG_JPEG_INTERCHANGE_FORMAT)
        .and_then(|e| entry_u32_value(prefix, endian, e));
    let jpeg_len = find_entry(&entries, TAG_JPEG_INTERCHANGE_FORMAT_LENGTH)
        .and_then(|e| entry_u32_value(prefix, endian, e));
    if let (Some(off), Some(len)) = (jpeg_off, jpeg_len) {
        fold_span(max_end, off, len);
    }

    let strip_off = find_entry(&entries, TAG_STRIP_OFFSETS);
    let strip_len = find_entry(&entries, TAG_STRIP_BYTE_COUNTS);
    if let (Some(off_entry), Some(len_entry)) = (strip_off, strip_len) {
        accumulate_strip_pairs(prefix, endian, off_entry, len_entry, max_end);
    }

    if follow_sub_ifds {
        if let Some(sub_entry) = find_entry(&entries, TAG_SUB_IFDS) {
            for sub_offset in entry_u32_values(prefix, endian, sub_entry) {
                accumulate_ifd_span(prefix, endian, u64::from(sub_offset), max_end, false);
            }
        }
    }
}

/// Read the entry count + all 12-byte entries of the IFD at `ifd_offset`.
/// Returns `None` if the offset, count field, or any entry is out of bounds.
fn read_ifd_entries(prefix: &[u8], endian: Endian, ifd_offset: u64) -> Option<Vec<RawEntry>> {
    let count = endian.read_u16(prefix, ifd_offset)?;
    let entries_start = ifd_offset.checked_add(2)?;

    let mut entries = Vec::with_capacity(usize::from(count));
    for i in 0..u64::from(count) {
        let entry_offset = entries_start.checked_add(i.checked_mul(IFD_ENTRY_SIZE)?)?;
        let tag = endian.read_u16(prefix, entry_offset)?;
        let type_ = endian.read_u16(prefix, entry_offset.checked_add(2)?)?;
        let cnt = endian.read_u32(prefix, entry_offset.checked_add(4)?)?;
        let value_offset = entry_offset.checked_add(8)?;
        // Confirm the value/offset slot itself is in bounds (4 bytes).
        let _ = endian.read_u32(prefix, value_offset)?;
        entries.push(RawEntry {
            tag,
            type_,
            count: cnt,
            value_offset,
        });
    }
    Some(entries)
}

fn find_entry(entries: &[RawEntry], tag: u16) -> Option<&RawEntry> {
    entries.iter().find(|e| e.tag == tag)
}

/// Interpret a single-value SHORT/LONG entry's inline value slot as a u32.
/// Returns `None` for unsupported types/counts or out-of-bounds reads.
fn entry_u32_value(prefix: &[u8], endian: Endian, entry: &RawEntry) -> Option<u32> {
    if entry.count != 1 {
        return None;
    }
    match entry.type_ {
        TYPE_SHORT => endian.read_u16(prefix, entry.value_offset).map(u32::from),
        TYPE_LONG => endian.read_u32(prefix, entry.value_offset),
        _ => None,
    }
}

/// Interpret a SHORT/LONG entry (any count) as a list of u32 values, resolving
/// the external-data offset when the inline 4-byte slot cannot hold them all.
/// Returns an empty vec (not `None`) on any bounds failure so callers can keep
/// iterating other tags.
fn entry_u32_values(prefix: &[u8], endian: Endian, entry: &RawEntry) -> Vec<u32> {
    let elem_size: u64 = match entry.type_ {
        TYPE_SHORT => 2,
        TYPE_LONG => 4,
        _ => return Vec::new(),
    };
    let count = u64::from(entry.count);
    let Some(total_size) = elem_size.checked_mul(count) else {
        return Vec::new();
    };

    let data_offset = if total_size <= 4 {
        entry.value_offset
    } else {
        let Some(off) = endian.read_u32(prefix, entry.value_offset) else {
            return Vec::new();
        };
        u64::from(off)
    };

    let mut out = Vec::with_capacity(entry.count as usize);
    for i in 0..count {
        let Some(elem_offset) = data_offset.checked_add(match i.checked_mul(elem_size) {
            Some(v) => v,
            None => return out,
        }) else {
            return out;
        };
        let value = match entry.type_ {
            TYPE_SHORT => endian.read_u16(prefix, elem_offset).map(u32::from),
            TYPE_LONG => endian.read_u32(prefix, elem_offset),
            _ => None,
        };
        match value {
            Some(v) => out.push(v),
            None => return out,
        }
    }
    out
}

/// Fold every (offset, length) pair from parallel StripOffsets/StripByteCounts
/// entries into `max_end`. The two arrays must be read with the same count;
/// pairs beyond the shorter array's length are ignored.
fn accumulate_strip_pairs(
    prefix: &[u8],
    endian: Endian,
    offsets_entry: &RawEntry,
    lengths_entry: &RawEntry,
    max_end: &mut Option<u64>,
) {
    let offsets = entry_u32_values(prefix, endian, offsets_entry);
    let lengths = entry_u32_values(prefix, endian, lengths_entry);
    for (off, len) in offsets.iter().zip(lengths.iter()) {
        fold_span(max_end, *off, *len);
    }
}

/// Fold one (offset, length) preview/strip pair into the running max `end`,
/// using checked arithmetic so an overflowing offset+length cannot wrap
/// around and silently produce a too-small span.
fn fold_span(max_end: &mut Option<u64>, offset: u32, length: u32) {
    let Some(end) = u64::from(offset).checked_add(u64::from(length)) else {
        return;
    };
    *max_end = Some(match *max_end {
        Some(existing) => existing.max(end),
        None => end,
    });
}

#[cfg(test)]
mod tests {
    use super::preview_span_end;

    // Build a minimal LE TIFF: header + IFD0 with one tag JPEGInterchangeFormat
    // (513, LONG, value=offset) and JPEGInterchangeFormatLength (514, LONG, value=len).
    fn tiny_tiff(preview_off: u32, preview_len: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"II"); // little-endian
        b.extend_from_slice(&42u16.to_le_bytes()); // magic
        b.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset = 8
                                                  // IFD0 at offset 8: entry count = 2
        b.extend_from_slice(&2u16.to_le_bytes());
        // entry: tag 513, type LONG(4), count 1, value
        let entry = |tag: u16, val: u32| {
            let mut e = Vec::new();
            e.extend_from_slice(&tag.to_le_bytes());
            e.extend_from_slice(&4u16.to_le_bytes()); // LONG
            e.extend_from_slice(&1u32.to_le_bytes()); // count
            e.extend_from_slice(&val.to_le_bytes()); // value/offset
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
