//! Off-thread warm-neighbor SOURCE prefetch (Task 7).
//!
//! Decodes each forward-biased filmstrip neighbor's edit-pipeline INPUT — the
//! demosaiced RAW buffer or the decoded Standard image — plus its persisted op
//! stack, off the UI thread, and delivers them via
//! [`crate::events::AppEvent::WarmSourceReady`]. Turning a delivered source
//! into a cached display texture (the render-thread warm-render) is Task 8;
//! this module only produces sources.
//!
//! **Bounded concurrency (memory, load-bearing):** mirrors
//! [`crate::develop::preview_cache::spawn_prefetch`]'s contract — a SINGLE
//! sequential `Background` job walks the neighbors one at a time, and each
//! neighbor's (large, camera-native) decoded buffer is dropped at the end of
//! its loop iteration, before the next neighbor decodes. The prefetch memory
//! peak is therefore one source buffer, never `neighbors.len()` of them
//! resident at once (CLAUDE.md responsiveness rule 1 + the develop-scroll
//! memory guardrail).
//!
//! **Pixel-consistency (load-bearing):** each neighbor is decoded via the SAME
//! path the on-screen full render uses, so a Task-8 warm-render built from
//! this source looks identical to a real cold open of that image:
//! * RAW: [`ferrolite_decode::decode_full`] + GPU RCD demosaic for RGGB
//!   sensors (else [`ferrolite_decode::QuadBin`]) +
//!   [`ferrolite_decode::apply_orientation_linear`] — exactly
//!   [`crate::viewer::load::spawn_full`]'s chain.
//! * Standard: [`ferrolite_decode::decode_preview`] (the full-res JPEG/PNG
//!   decode — a Standard image's tier-1 preview IS its full-resolution image)
//!   plus [`crate::viewer::load::preview_to_linear`] — exactly
//!   [`crate::viewer::load::spawn_preview`]'s chain.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferrolite_decode::{ColorProfile, DemosaicToRgb16f, QuadBin};
use ferrolite_gpu::GpuContext;
use ferrolite_image::{FileKind, LinearRgbaF32};
use ferrolite_jobs::{JobHandle, JobSystem, Priority};

use crate::events::AppEvent;

/// Decode + demosaic + upright a RAW neighbor exactly as
/// [`crate::viewer::load::spawn_full`] does: full decode, GPU RCD for RGGB
/// sensors else `QuadBin`, then `apply_orientation_linear`.
fn decode_raw_source(
    gpu: &GpuContext,
    path: &Path,
) -> Result<(LinearRgbaF32, ColorProfile), String> {
    let raw = ferrolite_decode::decode_full(path).map_err(|e| e.to_string())?;
    let color_profile = raw.color_profile.clone();
    let demosaiced = if raw.cfa_pattern == [0, 1, 1, 2] {
        ferrolite_pipeline::demosaic_rcd_gpu(
            gpu,
            &ferrolite_pipeline::CfaInput {
                pixels: &raw.pixels,
                width: raw.width,
                height: raw.height,
                cfa_pattern: raw.cfa_pattern,
                black_levels: raw.black_levels,
                white_level: raw.white_level,
                wb_coeffs: raw.wb_coeffs,
            },
        )
    } else {
        QuadBin.to_linear_rgba_f32(&raw)
    };
    let image = ferrolite_decode::apply_orientation_linear(demosaiced, raw.orientation);
    Ok((image, color_profile))
}

/// Decode a Standard neighbor exactly as [`crate::viewer::load::spawn_preview`]
/// does: the full-res decode, converted to display-linear. Standard images
/// carry no embedded camera calibration, so `color_profile` is the sRGB
/// fallback — the same value `ViewerState::color_profile` starts at and is
/// never overwritten for a Standard open (`apply_full_decoded`, which sets it,
/// only runs on the RAW `FullDecoded` path).
fn decode_standard_source(
    path: &Path,
    kind: FileKind,
) -> Result<(LinearRgbaF32, ColorProfile), String> {
    let image = ferrolite_decode::decode_preview(path, kind).map_err(|e| e.to_string())?;
    let linear = crate::viewer::load::preview_to_linear(&image);
    Ok((linear, ColorProfile::srgb_fallback()))
}

/// Spawn a SINGLE serialized `Priority::Background` job that walks `neighbors`
/// sequentially. For each neighbor, off-thread, it:
/// 1. Reads the neighbor's persisted `frl:ops` sidecar (mirrors
///    [`crate::develop::ops_persist::spawn_ops_read`]'s read logic; absent /
///    malformed / unknown-version resolves to `OpStack::default()`).
/// 2. Decodes + demosaics the neighbor's source via the on-screen full-render
///    path for its `FileKind` (see the module docs).
/// 3. Emits [`AppEvent::WarmSourceReady`].
///
/// The decoded source is dropped at the end of each loop iteration, before the
/// next neighbor decodes — bounding the prefetch's memory peak to one source
/// buffer (see the module-level bounded-concurrency note).
///
/// `gpu` is only touched for RAW neighbors on an RGGB sensor (GPU RCD); it is
/// an `Arc` clone of the SAME context the render thread already owns, never
/// rebuilt here (CLAUDE.md responsiveness rule 2).
///
/// Per-neighbor failure (decode/demosaic error) skips that neighbor and
/// continues; cancellation stops the whole walk. A prefetch failure must never
/// disturb the viewer, so it is only logged via `eprintln!`, never surfaced as
/// a user-facing event. Returns the single job handle (in a `Vec`, mirroring
/// `spawn_prefetch`) so the caller can cancel the whole walk on navigation.
pub fn spawn_warm_sources(
    jobs: &Arc<JobSystem>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    ctx: &egui::Context,
    neighbors: Vec<(i64, PathBuf, FileKind)>,
    gpu: Arc<GpuContext>,
) -> Vec<JobHandle> {
    let tx = tx.clone();
    let ctx = ctx.clone();
    let handle = jobs.submit(Priority::Background, move |cancel| {
        for (image_id, path, kind) in &neighbors {
            let image_id = *image_id;
            let kind = *kind;
            if cancel.is_cancelled() {
                return;
            }
            // Op-stack sidecar read (mirrors `ops_persist::spawn_ops_read`'s
            // read logic): absent/malformed/unknown-version -> default stack.
            let xmp = ferrolite_catalog::sidecar_path(path);
            let op_stack = ferrolite_catalog::read_ops(&xmp)
                .and_then(|p| ferrolite_pipeline::deserialize(&p))
                .unwrap_or_default();

            if cancel.is_cancelled() {
                return;
            }

            let decoded = match kind {
                FileKind::Raw => decode_raw_source(&gpu, path),
                FileKind::Standard => decode_standard_source(path, kind),
            };
            let (source, color_profile) = match decoded {
                Ok(pair) => pair,
                Err(err) => {
                    eprintln!("warm prefetch: decode failed for #{image_id}: {err}");
                    continue;
                }
            };

            let _ = tx.send(AppEvent::WarmSourceReady {
                image_id,
                source: Arc::new(source),
                op_stack,
                kind,
                color_profile,
            });
            ctx.request_repaint();
            // `source` is dropped here (end of iteration), before the next
            // neighbor decodes — the bounded-concurrency contract.
        }
    });
    vec![handle]
}
