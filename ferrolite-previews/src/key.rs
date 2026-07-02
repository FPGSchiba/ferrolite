//! `PreviewKey` and the dependency-free FNV-1a-64 digest used to name
//! cache entries on disk.

/// FNV-1a-64 offset basis, per the canonical FNV specification.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a-64 prime, per the canonical FNV specification.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a-64 over arbitrary bytes (dependency-free, stable across runs and
/// crate versions — do not change this algorithm without also invalidating
/// every on-disk cache entry keyed by it).
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Hash any serde-serializable value via canonical `serde_json` bytes,
/// then `fnv1a_64`.
///
/// # Panics
///
/// Panics if `value` fails to serialize to JSON. All expected inputs
/// (preview keys, op stacks, color profile descriptors) are plain data
/// structures that always serialize successfully; a failure here indicates
/// a programming error (e.g. a `Serialize` impl that errors), not bad
/// external input.
pub fn hash_serde<T: serde::Serialize>(value: &T) -> u64 {
    let bytes = serde_json::to_vec(value).expect("value must be JSON-serializable");
    fnv1a_64(&bytes)
}

/// Identifies one cached preview render: the source file's identity, the
/// edit stack and color pipeline that produced it, and the pipeline
/// parameters that affect output pixels. Two `PreviewKey`s with the same
/// [`PreviewKey::digest`] are expected to produce byte-identical previews.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct PreviewKey {
    /// Source RAW file size in bytes, from filesystem metadata.
    pub file_size: u64,
    /// Source RAW file modification time in nanoseconds since the Unix
    /// epoch, from filesystem metadata.
    pub file_mtime_ns: i64,
    /// Stable hash of the serialized edit/op stack applied to the image.
    pub op_stack_hash: u64,
    /// Working color space identifier (small enum discriminant).
    pub working_space: u8,
    /// Stable hash of the active color profile (camera/output ICC, etc).
    pub color_profile_hash: u64,
    /// Long edge (px) the preview was rendered at.
    pub preview_long_edge: u32,
    /// Pipeline schema version active when the preview was rendered.
    pub schema_version: u32,
}

impl PreviewKey {
    /// 16-hex-char FNV-1a-64 digest over the canonical little-endian bytes
    /// of every field, in declaration order. Used as the cache entry's
    /// on-disk filename stem.
    pub fn digest(&self) -> String {
        let mut bytes = Vec::with_capacity(8 + 8 + 8 + 1 + 8 + 4 + 4);
        bytes.extend_from_slice(&self.file_size.to_le_bytes());
        bytes.extend_from_slice(&self.file_mtime_ns.to_le_bytes());
        bytes.extend_from_slice(&self.op_stack_hash.to_le_bytes());
        bytes.extend_from_slice(&self.working_space.to_le_bytes());
        bytes.extend_from_slice(&self.color_profile_hash.to_le_bytes());
        bytes.extend_from_slice(&self.preview_long_edge.to_le_bytes());
        bytes.extend_from_slice(&self.schema_version.to_le_bytes());
        format!("{:016x}", fnv1a_64(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_key() -> PreviewKey {
        PreviewKey {
            file_size: 12_345_678,
            file_mtime_ns: 1_700_000_000_000_000_000,
            op_stack_hash: 0xdead_beef_cafe_f00d,
            working_space: 2,
            color_profile_hash: 0x1122_3344_5566_7788,
            preview_long_edge: 2048,
            schema_version: 1,
        }
    }

    #[test]
    fn digest_is_stable() {
        let key = base_key();
        // Pinned from the actual first passing run (Step 4 of the brief).
        assert_eq!(key.digest(), "b1c8436d6b494b6a");
    }

    #[test]
    fn digest_changes_when_any_field_changes() {
        let base = base_key();
        let base_digest = base.digest();

        let mutated: Vec<PreviewKey> = vec![
            PreviewKey {
                file_size: base.file_size + 1,
                ..base.clone()
            },
            PreviewKey {
                file_mtime_ns: base.file_mtime_ns + 1,
                ..base.clone()
            },
            PreviewKey {
                op_stack_hash: base.op_stack_hash + 1,
                ..base.clone()
            },
            PreviewKey {
                working_space: base.working_space.wrapping_add(1),
                ..base.clone()
            },
            PreviewKey {
                color_profile_hash: base.color_profile_hash + 1,
                ..base.clone()
            },
            PreviewKey {
                preview_long_edge: base.preview_long_edge + 1,
                ..base.clone()
            },
            PreviewKey {
                schema_version: base.schema_version + 1,
                ..base.clone()
            },
        ];

        assert_eq!(mutated.len(), 7, "one mutation per PreviewKey field");
        for (i, key) in mutated.iter().enumerate() {
            assert_ne!(
                key.digest(),
                base_digest,
                "mutating field index {i} did not change the digest"
            );
        }
    }

    #[test]
    fn hash_serde_is_order_stable() {
        let a = base_key();
        let a_again = base_key();
        let mut b = base_key();
        b.file_size += 1;

        assert_eq!(hash_serde(&a), hash_serde(&a_again));
        assert_ne!(hash_serde(&a), hash_serde(&b));
    }
}
