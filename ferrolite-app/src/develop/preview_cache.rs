//! Preview-cache key assembly + off-thread write-back for the develop viewer.
//!
//! Task 5 wires the pure `ferrolite-previews` crate into the app: it builds a
//! [`PreviewKey`] from the open RAW's file identity + edit stack + color
//! pipeline, and — on a qualifying open (RAW or Standard) — spawns a
//! `Background` job that encodes the identity (unedited) color-managed render
//! to a 2048px sRGB JPEG and stores it under that key, then trims the cache to
//! its cap.
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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use ferrolite_color::{Mat3, WorkingSpace};
use ferrolite_decode::{decode_color_profile, ColorProfile, DemosaicToRgb16f, QuadBin};
use ferrolite_image::LinearRgbaF32;
use ferrolite_jobs::{JobHandle, JobSystem, Priority};
use ferrolite_pipeline::OpStack;
use ferrolite_previews::{
    decode_srgb_jpeg, encode_srgb_jpeg, fnv1a_64, hash_serde, PreviewKey, PreviewStore,
    PIPELINE_SCHEMA_VERSION, PREVIEW_LONG_EDGE,
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
/// Two conditions must both hold (format-agnostic — RAW and Standard/JPG are
/// treated equally, per the tiered-cache design: JPGs are first-class camera
/// originals, not quick looks):
/// * **default op stack** — the payload encoded is the *identity* render but the
///   key hashes the *actual* stack, so caching under an edited key would later
///   reveal the wrong image (see the module-level correctness guard).
/// * **cache miss** (`is_cache_miss`) — a cache *hit* already has the entry on
///   disk, so re-encoding it would be pure waste. The read path threads the real
///   miss flag here (`v.cache_write_back`).
pub fn should_write_back(op_stack: &OpStack, is_cache_miss: bool) -> bool {
    *op_stack == OpStack::default() && is_cache_miss
}

/// Look up `key` in `store`; on a hit decode the cached JPEG and convert it from
/// 8-bit sRGB to display-linear (reusing [`crate::viewer::load::preview_to_linear`]),
/// so the result matches `PreviewReady`'s shape for reveal via the Improvement-1
/// sRGB path (`reveal_srgb_preview`). Returns `None` on a miss or a decode error
/// — a read failure must always resolve to a miss so the viewer falls through to
/// the full-decode path and never gets stuck. Pure (no threads / GPU / UI).
pub fn read_cached_preview(store: &PreviewStore, key: &PreviewKey) -> Option<LinearRgbaF32> {
    let bytes = store.get(key)?;
    let imgbuf = decode_srgb_jpeg(&bytes).ok()?;
    Some(crate::viewer::load::preview_to_linear(&imgbuf))
}

/// Spawn a `Visible`-priority job that consults the preview cache for the open
/// RAW and reports the outcome as a [`AppEvent::PreviewCacheHit`] (with the
/// decoded display-linear buffer, ready for `reveal_srgb_preview`) or a
/// [`AppEvent::PreviewCacheMiss`]. It gates the reveal, so it rides the open
/// critical path at the same priority as the full decode.
///
/// The [`PreviewKey`] is built INSIDE the job, not on the UI thread: it needs
/// the camera [`ColorProfile`], obtained via [`decode_color_profile`] (a cheap
/// dummy `raw_image`, NO demosaic) — a multi-millisecond metadata decode that
/// must never run on the UI thread (CLAUDE.md threading rule 1). The JPEG decode
/// and sRGB→linear conversion also run here. The UI thread only clones
/// `path`/`op_stack` in.
///
/// Any failure (cancelled, profile decode error, key error, cache miss, decode
/// error) resolves to `PreviewCacheMiss` so the viewer never stalls waiting on a
/// reveal that will not come. Returns the [`JobHandle`] so the caller can cancel
/// the read when the user scrubs past this image.
#[allow(clippy::too_many_arguments)]
pub fn spawn_cache_read(
    jobs: &Arc<JobSystem>,
    store: Arc<PreviewStore>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    path: PathBuf,
    op_stack: OpStack,
    working_space: WorkingSpace,
) -> JobHandle {
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Visible, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        // Build the key off-thread: the profile decode is a metadata-only dummy
        // decode (no demosaic), still too heavy for the UI thread.
        let outcome = decode_color_profile(&path)
            .ok()
            .and_then(|profile| key_for(&path, &op_stack, working_space, &profile).ok())
            .and_then(|key| read_cached_preview(&store, &key));
        let event = match outcome {
            Some(linear) => AppEvent::PreviewCacheHit { image_id, linear },
            None => AppEvent::PreviewCacheMiss { image_id },
        };
        let _ = tx.send(event);
        ctx.request_repaint();
    })
}

/// Spawn a `Background` job that assembles the [`PreviewKey`] off-thread,
/// encodes `render` (with `display_matrix` applied) to a 2048px sRGB JPEG,
/// stores it under that key, and evicts the cache down to `cap` bytes. The key
/// assembly ([`key_for`], which does an `fs::metadata` stat) and all
/// encode / disk I/O happen on the job thread — the UI thread only clones the
/// key inputs (`path`/`op_stack`/`working_space`/`color_profile`) and the
/// render `Arc` (a cheap refcount bump, no O(pixels) memcpy) in. A cache
/// failure is logged and dropped: it must never disturb the viewer. On success
/// a [`AppEvent::PreviewCacheWritten`] is emitted (for metrics/tests) and a
/// repaint requested.
///
/// `render` is an [`Arc`] over the SAME camera-native buffer the reveal uses
/// (`v.raw_preview_source`), so the write-back never copies the demosaic a
/// second time. `key_for` here is the shared assembler used by the read and
/// prefetch paths, so the write-back key is identical to what those produce.
#[allow(clippy::too_many_arguments)]
pub fn spawn_cache_write(
    jobs: &Arc<JobSystem>,
    store: Arc<PreviewStore>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    ctx: &egui::Context,
    path: PathBuf,
    op_stack: OpStack,
    working_space: WorkingSpace,
    color_profile: ColorProfile,
    render: Arc<LinearRgbaF32>,
    display_matrix: Mat3,
    cap: u64,
    image_id: i64,
) {
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Background, move |_cancel| {
        // Assemble the key off-thread: `key_for` does an `fs::metadata` stat,
        // which must never run on the UI thread (CLAUDE.md threading rule 1).
        let key = match key_for(&path, &op_stack, working_space, &color_profile) {
            Ok(key) => key,
            Err(err) => {
                eprintln!("preview cache: key_for failed for #{image_id}: {err}");
                return;
            }
        };
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

/// The ordered neighbor image ids within `radius` of `current` in `ids` (both
/// directions), excluding `current`, clamped at the list ends.
///
/// Finds `current`'s position and returns the positional (left-to-right)
/// window `[pos-radius, pos+radius]` intersected with `[0, len)`, minus `pos`
/// itself. If `current` is absent from `ids` (or `ids` is empty) the window
/// has no anchor, so the result is empty — there is nothing meaningful to
/// prefetch around a selection that isn't in the list.
pub fn prefetch_targets(ids: &[i64], current: i64, radius: usize) -> Vec<i64> {
    let Some(pos) = ids.iter().position(|id| *id == current) else {
        return Vec::new();
    };
    let start = pos.saturating_sub(radius);
    let end = (pos + radius).min(ids.len() - 1);
    (start..=end)
        .filter(|&i| i != pos)
        .map(|i| ids[i])
        .collect()
}

/// Spawn a SINGLE `Priority::Background` job that walks `neighbors` (RAW images
/// only, resolved by the caller) sequentially: for each not already cached under
/// its DEFAULT (identity) preview key, it decodes + demosaics + color-manages +
/// encodes the identity render and stores it, mirroring the on-open write-back
/// chain (`apply_full_decoded` in `app.rs`) EXACTLY so a prefetched entry is
/// byte-for-byte what that write-back would have produced.
///
/// **Bounded concurrency (memory, load-bearing):** one sequential job — NOT one
/// job per neighbor. Each neighbor's full-res demosaic (`QuadBin` → a ~400 MB
/// f32 buffer for a 24 MP frame) is dropped at the end of its loop iteration,
/// before the next neighbor decodes, so the prefetch peak is a SINGLE such
/// buffer rather than `neighbors.len()` of them resident at once. That
/// concurrent pile-up was the dominant driver of the develop-scroll RSS
/// high-water mark; radius/coverage is unchanged, only concurrency is bounded.
///
/// Prefetch keys by the default op stack — it does NOT read each neighbor's
/// edit sidecar, so an edited neighbor's default-keyed entry is simply never
/// requested until reset (a deliberate, harmless miss; see the module docs).
///
/// Per-neighbor failure (profile-decode error, key error, already cached,
/// full-decode error, encode error, store error) skips that neighbor and
/// continues; cancellation stops the whole walk. A prefetch must never disturb
/// the viewer, so failures are only logged via `eprintln!`, never surfaced as an
/// event. Returns the single job handle (in a `Vec`) so the caller can cancel
/// the whole walk on navigation.
pub fn spawn_prefetch(
    jobs: &Arc<JobSystem>,
    store: Arc<PreviewStore>,
    ctx: &egui::Context,
    neighbors: &[(i64, PathBuf)],
    working_space: WorkingSpace,
    cap: u64,
) -> Vec<JobHandle> {
    let neighbors = neighbors.to_vec();
    let ctx = ctx.clone();
    let handle = jobs.submit(Priority::Background, move |cancel| {
        for (image_id, path) in &neighbors {
            let image_id = *image_id;
            if cancel.is_cancelled() {
                return;
            }
            let Ok(profile) = decode_color_profile(path) else {
                continue;
            };
            let Ok(key) = key_for(path, &OpStack::default(), working_space, &profile) else {
                continue;
            };
            if store.contains(&key) {
                continue; // already cached — skip the expensive decode
            }
            if cancel.is_cancelled() {
                return;
            }
            let Ok(raw) = ferrolite_decode::decode_full(path) else {
                continue;
            };
            // Demosaic (QuadBin — this is the tier-1 reveal/prefetch cache only;
            // the on-screen full tier uses GPU RCD via `spawn_full`) + upright.
            // This ~400 MB f32 `image` is dropped at the end of the iteration,
            // before the next neighbor decodes (bounded-concurrency contract).
            let image = ferrolite_decode::apply_orientation_linear(
                QuadBin.to_linear_rgba_f32(&raw),
                raw.orientation,
            );
            // Identity display matrix: camera→working then working→display,
            // matching the write-back composition in `apply_full_decoded`.
            let cam = ferrolite_color::normalize_neutral(ferrolite_color::camera_to_working(
                raw.color_profile.xyz_to_cam,
                ferrolite_color::Xy {
                    x: raw.color_profile.white_xy[0],
                    y: raw.color_profile.white_xy[1],
                },
                working_space,
            ));
            let display_matrix = ferrolite_color::mul_mat3(
                &ferrolite_color::working_to_display(working_space),
                &cam,
            );
            let jpeg = match encode_srgb_jpeg(
                &image,
                display_matrix,
                PREVIEW_LONG_EDGE,
                PREVIEW_JPEG_QUALITY,
            ) {
                Ok(jpeg) => jpeg,
                Err(err) => {
                    eprintln!("preview prefetch: encode failed for #{image_id}: {err}");
                    continue;
                }
            };
            if let Err(err) = store.put(&key, &jpeg) {
                eprintln!("preview prefetch: put failed for #{image_id}: {err}");
                continue;
            }
            if let Err(err) = store.evict_to(cap) {
                eprintln!("preview prefetch: evict_to failed after #{image_id}: {err}");
            }
            ctx.request_repaint();
        }
    });
    vec![handle]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sample_profile() -> ColorProfile {
        let xyz_to_cam = [[1.0, 0.1, 0.2], [0.3, 1.1, 0.4], [0.5, 0.6, 1.2]];
        let white_xy = [0.3127, 0.3290];
        ColorProfile {
            xyz_to_cam,
            white_xy,
            is_fallback: false,
            calibrations: vec![ferrolite_decode::CameraCalibration {
                xyz_to_cam,
                white_xy,
            }],
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
    fn write_back_gated_on_default_stack_and_miss_for_any_kind() {
        let default_stack = OpStack::default();
        let edited_stack = default_stack.set_op(ferrolite_pipeline::Op::Exposure(
            ferrolite_pipeline::Exposure { ev: 0.5 },
        ));

        // Default stack + cache MISS -> write back (the only qualifying case).
        // Now format-agnostic: JPGs are first-class originals and cache the same
        // way RAWs do (spec: JPG Tier-1 write-back).
        assert!(should_write_back(&default_stack, true));
        // Default stack + cache HIT -> SKIP: the entry already exists on disk, so
        // re-encoding it is pure waste.
        assert!(!should_write_back(&default_stack, false));
        // Edited stack -> SKIP (guard: an identity render under an edited key
        // would reveal the wrong image), regardless of the miss flag.
        assert!(!should_write_back(&edited_stack, true));
    }

    #[test]
    fn read_cached_preview_hits_seeded_entry_and_misses_absent() {
        // A tiny known render, JPEG-encoded with the real codec and seeded into a
        // temp store under `key`, must read back as a display-linear buffer with
        // the (downscaled) cached dims; an absent key must read back as `None`.
        let dir = std::env::temp_dir().join(format!(
            "ferrolite-cacheread-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let store = PreviewStore::new(&dir).expect("store creates its dir");

        let path = temp_raw_file("cacheread");
        let key = key_for(
            &path,
            &OpStack::default(),
            WorkingSpace::Rec2020,
            &sample_profile(),
        )
        .expect("key_for succeeds");

        // 8×4 solid mid-gray working-linear render (long edge 8 < 2048 → no
        // downscale, so decoded dims equal the render dims).
        let mut px = Vec::with_capacity(8 * 4 * 4);
        for _ in 0..(8 * 4) {
            px.extend_from_slice(&[0.18, 0.18, 0.18, 1.0]);
        }
        let render = LinearRgbaF32::new(8, 4, px).expect("valid render");
        let jpeg = encode_srgb_jpeg(&render, ferrolite_color::identity(), PREVIEW_LONG_EDGE, 90)
            .expect("encode succeeds");
        store.put(&key, &jpeg).expect("seed the cache");

        let hit = read_cached_preview(&store, &key).expect("seeded key must hit");
        assert_eq!((hit.width, hit.height), (8, 4), "cached preview dims");
        assert!(hit.pixels[3] > 0.99, "alpha is opaque after sRGB→linear");

        // A key for a different file (different size/mtime) must miss.
        let other_path = temp_raw_file("cacheread-absent");
        let absent_key = key_for(
            &other_path,
            &OpStack::default(),
            WorkingSpace::Rec2020,
            &sample_profile(),
        )
        .expect("key_for succeeds");
        assert!(
            read_cached_preview(&store, &absent_key).is_none(),
            "absent key must miss"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&other_path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prefetch_targets_middle_returns_both_sides_within_radius() {
        let ids = [10, 20, 30, 40, 50];
        // current = 30 (pos 2), radius 1 → neighbors at pos 1 and 3, excluding pos 2.
        assert_eq!(prefetch_targets(&ids, 30, 1), vec![20, 40]);
        // radius 2 → the full window minus current.
        assert_eq!(prefetch_targets(&ids, 30, 2), vec![10, 20, 40, 50]);
    }

    #[test]
    fn prefetch_targets_clamps_at_both_ends() {
        let ids = [10, 20, 30, 40, 50];
        // current = first element: no left side, clamp at 0.
        assert_eq!(prefetch_targets(&ids, 10, 2), vec![20, 30]);
        // current = last element: no right side, clamp at len-1.
        assert_eq!(prefetch_targets(&ids, 50, 2), vec![30, 40]);
    }

    #[test]
    fn prefetch_targets_radius_larger_than_remaining_clamps_without_panicking() {
        let ids = [10, 20, 30];
        // radius far exceeds the list length in both directions.
        assert_eq!(prefetch_targets(&ids, 10, 100), vec![20, 30]);
        assert_eq!(prefetch_targets(&ids, 30, 100), vec![10, 20]);
    }

    #[test]
    fn prefetch_targets_current_absent_is_empty() {
        let ids = [10, 20, 30];
        assert!(prefetch_targets(&ids, 999, 2).is_empty());
        assert!(prefetch_targets(&[], 1, 2).is_empty());
    }

    #[test]
    fn prefetch_targets_excludes_current() {
        let ids = [10, 20, 30, 40, 50];
        let targets = prefetch_targets(&ids, 30, 2);
        assert!(
            !targets.contains(&30),
            "current id must never be its own neighbor"
        );
    }
}
