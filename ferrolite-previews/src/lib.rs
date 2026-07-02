//! `ferrolite-previews` — on-disk cache of downscaled, color-managed RAW
//! previews, keyed by a stable digest of the inputs that affect the
//! rendered pixels (source file identity + edit stack + color pipeline).
//!
//! This crate is intentionally dependency-free beyond `serde`/`serde_json`
//! for the key/digest layer implemented here (Task 1). Later tasks add the
//! JPEG codec, the on-disk store, and LRU eviction.

mod key;

pub use key::{fnv1a_64, hash_serde, PreviewKey};

/// Long edge (in pixels) that cached previews are downscaled to.
pub const PREVIEW_LONG_EDGE: u32 = 2048;

/// Default cap (in bytes) for the on-disk preview cache.
pub const DEFAULT_CACHE_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Schema version for the preview pipeline; bump to invalidate all cached
/// previews when the rendering pipeline changes in a way that affects
/// output pixels.
pub const PIPELINE_SCHEMA_VERSION: u32 = 1;
