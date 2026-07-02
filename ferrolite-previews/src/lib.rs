//! `ferrolite-previews` — on-disk cache of downscaled, color-managed RAW
//! previews, keyed by a stable digest of the inputs that affect the
//! rendered pixels (source file identity + edit stack + color pipeline).
//!
//! The key/digest layer (Task 1) is dependency-free beyond `serde`/
//! `serde_json`; the 8-bit sRGB JPEG codec (Task 2) additionally depends on
//! `ferrolite-image`, `ferrolite-color`, and `image`. Later tasks add the
//! on-disk store and LRU eviction.

mod codec;
mod key;

pub use codec::{decode_srgb_jpeg, encode_srgb_jpeg, PreviewCodecError};
pub use key::{fnv1a_64, hash_serde, PreviewKey};

/// Long edge (in pixels) that cached previews are downscaled to.
pub const PREVIEW_LONG_EDGE: u32 = 2048;

/// Default cap (in bytes) for the on-disk preview cache.
pub const DEFAULT_CACHE_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Schema version for the preview pipeline; bump to invalidate all cached
/// previews when the rendering pipeline changes in a way that affects
/// output pixels.
pub const PIPELINE_SCHEMA_VERSION: u32 = 1;
