//! Preview-cache key assembly + off-thread write-back for the develop viewer.
//!
//! Task 5 wires the pure `ferrolite-previews` crate into the app: it builds a
//! [`PreviewKey`] from the open RAW's file identity + edit stack + color
//! pipeline, and — on a qualifying RAW open — spawns a `Background` job that
//! encodes the identity (unedited) color-managed render to a 2048px sRGB JPEG
//! and stores it under that key, then trims the cache to its cap.
//!
//! ## Correctness guard (load-bearing)
//!
//! [`key_for`] hashes the *actual* op stack, but the payload encoded here is the
//! *identity* (camera→working→display) render computed on the CPU from the
//! demosaiced buffer — never the GPU op-stack result. Storing an identity render
//! under an *edited* key would make a later cache hit reveal the wrong
//! (unedited) image. So write-back is gated on the stack being `OpStack::default()`
//! (see [`should_write_back`]). Edited images are a deliberate cache miss (live
//! render every visit) until a later task reads back the real GPU render.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use ferrolite_color::{Mat3, WorkingSpace};
use ferrolite_decode::ColorProfile;
use ferrolite_image::LinearRgbaF32;
use ferrolite_jobs::{JobSystem, Priority};
use ferrolite_pipeline::OpStack;
use ferrolite_previews::{
    encode_srgb_jpeg, fnv1a_64, hash_serde, PreviewKey, PreviewStore, PIPELINE_SCHEMA_VERSION,
    PREVIEW_LONG_EDGE,
};

use crate::events::AppEvent;

/// JPEG quality (0–100) for cached previews. High enough that the cached
/// reveal is visually indistinguishable from the live render; low enough to
/// keep entries small for the LRU cap.
const PREVIEW_JPEG_QUALITY: u8 = 90;

/// Stable hash of a [`ColorProfile`]. `ColorProfile` is **not** `Serialize`, so
/// (per the Task 1 helper note) we hash its raw little-endian bytes: the nine
/// `xyz_to_cam` floats, the two `white_xy` floats, then `is_fallback` (so the
/// fallback profile keys distinctly from a real one with the same numbers).
fn hash_color_profile(profile: &ColorProfile) -> u64 {
    let mut bytes = Vec::with_capacity(9 * 4 + 2 * 4 + 1);
    for row in &profile.xyz_to_cam {
        for v in row {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    for v in &profile.white_xy {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes.push(profile.is_fallback as u8);
    fnv1a_64(&bytes)
}

/// Build the [`PreviewKey`] for the currently-open RAW image from its path,
/// edit stack, working space, and color profile. `file_size`/`file_mtime_ns`
/// come from `fs::metadata`; a metadata error is surfaced as an [`io::Error`]
/// (the caller treats it as "cannot cache" and skips write-back).
pub fn key_for(
    path: &Path,
    op_stack: &OpStack,
    working_space: WorkingSpace,
    color_profile: &ColorProfile,
) -> io::Result<PreviewKey> {
    let meta = std::fs::metadata(path)?;
    let file_size = meta.len();
    let file_mtime_ns = match meta.modified()?.duration_since(UNIX_EPOCH) {
        Ok(since_epoch) => i64::try_from(since_epoch.as_nanos()).unwrap_or(i64::MAX),
        Err(before_epoch) => i64::try_from(before_epoch.duration().as_nanos())
            .map(|nanos| nanos.saturating_neg())
            .unwrap_or(i64::MIN),
    };
    Ok(PreviewKey {
        file_size,
        file_mtime_ns,
        op_stack_hash: hash_serde(op_stack),
        working_space: working_space as u8,
        color_profile_hash: hash_color_profile(color_profile),
        preview_long_edge: PREVIEW_LONG_EDGE,
        schema_version: PIPELINE_SCHEMA_VERSION,
    })
}

/// Whether an open should write its render back to the preview cache.
///
/// Only RAW opens with the **default** op stack qualify: Task 5 caches the
/// identity render but keys by the actual stack, so caching under an edited key
/// would later reveal the wrong image (see the module-level correctness guard).
/// A later task will thread a real "cache miss" flag through the read path; for
/// now every qualifying RAW open is treated as a miss.
pub fn should_write_back(is_raw: bool, op_stack: &OpStack) -> bool {
    is_raw && *op_stack == OpStack::default()
}

/// Spawn a `Background` job that encodes `render` (with `display_matrix`
/// applied) to a 2048px sRGB JPEG, stores it under `key`, and evicts the cache
/// down to `cap` bytes. All encode / disk I/O happens on the job thread — the
/// UI thread only builds the key and clones the render/matrix in. A cache
/// failure is logged and dropped: it must never disturb the viewer. On success
/// a [`AppEvent::PreviewCacheWritten`] is emitted (for metrics/tests) and a
/// repaint requested.
#[allow(clippy::too_many_arguments)]
pub fn spawn_cache_write(
    jobs: &Arc<JobSystem>,
    store: Arc<PreviewStore>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    ctx: &egui::Context,
    key: PreviewKey,
    render: LinearRgbaF32,
    display_matrix: Mat3,
    cap: u64,
    image_id: i64,
) {
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Background, move |_cancel| {
        let jpeg = match encode_srgb_jpeg(
            &render,
            display_matrix,
            PREVIEW_LONG_EDGE,
            PREVIEW_JPEG_QUALITY,
        ) {
            Ok(jpeg) => jpeg,
            Err(err) => {
                eprintln!("preview cache: encode failed for #{image_id}: {err}");
                return;
            }
        };
        if let Err(err) = store.put(&key, &jpeg) {
            eprintln!("preview cache: put failed for #{image_id}: {err}");
            return;
        }
        if let Err(err) = store.evict_to(cap) {
            // Non-fatal: the entry is written; eviction just didn't run to cap.
            eprintln!("preview cache: evict_to failed after #{image_id}: {err}");
        }
        let _ = tx.send(AppEvent::PreviewCacheWritten { image_id });
        ctx.request_repaint();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sample_profile() -> ColorProfile {
        ColorProfile {
            xyz_to_cam: [[1.0, 0.1, 0.2], [0.3, 1.1, 0.4], [0.5, 0.6, 1.2]],
            white_xy: [0.3127, 0.3290],
            is_fallback: false,
        }
    }

    /// Writes a small temp file so `fs::metadata` succeeds, returning its path.
    /// The file is left on disk for the test's duration (OS temp dir); a unique
    /// name per call avoids collisions between concurrent test threads.
    fn temp_raw_file(label: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ferrolite-previewkey-test-{label}-{}-{nanos}-{seq}.raw",
            std::process::id()
        ));
        let mut f = std::fs::File::create(&path).expect("create temp raw file");
        f.write_all(b"not a real raw, just bytes for metadata")
            .expect("write temp raw file");
        path
    }

    #[test]
    fn key_for_is_stable_and_input_sensitive() {
        let path = temp_raw_file("stable");
        let stack = OpStack::default();
        let ws = WorkingSpace::Rec2020;
        let profile = sample_profile();

        // Same inputs → equal key.
        let a = key_for(&path, &stack, ws, &profile).expect("key_for succeeds");
        let b = key_for(&path, &stack, ws, &profile).expect("key_for succeeds");
        assert_eq!(a, b, "identical inputs must produce an identical key");

        // file_size / file_mtime_ns are populated from real metadata.
        assert!(a.file_size > 0, "file_size must come from fs::metadata");
        assert_ne!(a.file_mtime_ns, 0, "file_mtime_ns must be populated");
        assert_eq!(a.preview_long_edge, PREVIEW_LONG_EDGE);
        assert_eq!(a.schema_version, PIPELINE_SCHEMA_VERSION);
        assert_eq!(a.working_space, WorkingSpace::Rec2020 as u8);

        // Different op stack → different key.
        let edited = stack.set_op(ferrolite_pipeline::Op::Exposure(
            ferrolite_pipeline::Exposure { ev: 1.0 },
        ));
        let a_edited = key_for(&path, &edited, ws, &profile).expect("key_for succeeds");
        assert_ne!(
            a.op_stack_hash, a_edited.op_stack_hash,
            "a different op stack must change op_stack_hash"
        );
        assert_ne!(a, a_edited, "a different op stack must change the key");

        // Different working space → different key.
        let a_srgb =
            key_for(&path, &stack, WorkingSpace::Srgb, &profile).expect("key_for succeeds");
        assert_ne!(
            a.working_space, a_srgb.working_space,
            "a different working space must change the discriminant"
        );
        assert_ne!(a, a_srgb, "a different working space must change the key");

        // Different color profile → different key.
        let mut other_profile = sample_profile();
        other_profile.white_xy = [0.3457, 0.3585];
        let a_other = key_for(&path, &stack, ws, &other_profile).expect("key_for succeeds");
        assert_ne!(
            a.color_profile_hash, a_other.color_profile_hash,
            "a different color profile must change color_profile_hash"
        );
        assert_ne!(a, a_other, "a different color profile must change the key");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn color_profile_fallback_flag_changes_hash() {
        let mut real = sample_profile();
        real.is_fallback = false;
        let mut fallback = sample_profile();
        fallback.is_fallback = true;
        assert_ne!(
            hash_color_profile(&real),
            hash_color_profile(&fallback),
            "is_fallback must be part of the color-profile hash"
        );
    }

    #[test]
    fn write_back_only_for_raw_default_stack() {
        let default_stack = OpStack::default();
        let edited_stack = default_stack.set_op(ferrolite_pipeline::Op::Exposure(
            ferrolite_pipeline::Exposure { ev: 0.5 },
        ));

        // RAW + default stack → write back (the only qualifying case).
        assert!(should_write_back(true, &default_stack));
        // RAW + edited stack → SKIP (guard: identity render under an edited key
        // would reveal the wrong image).
        assert!(!should_write_back(true, &edited_stack));
        // Non-RAW → never (standard images do not reach the RAW reveal path).
        assert!(!should_write_back(false, &default_stack));
    }
}
