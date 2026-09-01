//! Batch export orchestration (spec §8.4). A **single** `ferrolite-export`
//! Background job processes the whole queue **one image at a time** (bounded
//! concurrency — see `spawn_batch` for why). Each item decodes ON THE WORKER
//! THREAD (never the UI thread), builds the GPU pyramid, computes camera→working
//! from the decoded ColorProfile, and renders each item's PERSISTED edit stack
//! (read from its XMP sidecar via `stack_for_item`), so a batch export matches
//! what the grid and Develop show. This was previously hardcoded to
//! `OpStack::default()`, which silently exported every image unedited once
//! sidecar persistence shipped (P7 design §7).

use std::path::PathBuf;
use std::sync::Arc;

use ferrolite_color::WorkingSpace;
use ferrolite_decode::{ColorProfile, DemosaicToRgb16f, Rcd};
use ferrolite_export::{run_export, ExportOptions, ExportRequest};
use ferrolite_gpu::GpuContext;
use ferrolite_image::{FileKind, LinearRgbaF32};
use ferrolite_jobs::{CancelToken, JobHandle, Priority};
use ferrolite_lens::LensfunDb;
use ferrolite_pipeline::{GpuPyramidSource, OpStack};

use crate::events::AppEvent;
use crate::state::AppState;

/// One image to export in a batch. `dest` is the final, collision-resolved path.
#[derive(Debug, Clone)]
pub struct BatchItem {
    pub image_id: i64,
    pub path: PathBuf,
    pub kind: FileKind,
    pub dest: PathBuf,
}

/// Submit the batch as a **single** Background job that processes items **one at
/// a time**. Returns the one job handle (for cancellation).
///
/// Concurrency is deliberately bounded to one in-flight export: each item does a
/// full-res tiled render on eframe's *shared* wgpu device plus a CPU-heavy encode,
/// so running several at once saturated the GPU/CPU and starved the UI thread
/// (the app went "Not Responding" during an 8-image AVIF batch). One-at-a-time
/// honors the "export never contends with the UI" intent (spec §8.1) — a single
/// rav1e encode already uses the whole CPU, so wall-clock barely changes while
/// responsiveness is restored. See CLAUDE.md "GPU work … must be bounded".
pub fn spawn_batch(
    state: &AppState,
    egui_ctx: &egui::Context,
    gpu: Arc<GpuContext>,
    items: Vec<BatchItem>,
    working_space: WorkingSpace,
    options: ExportOptions,
) -> Vec<JobHandle> {
    let tx = state.tx.clone();
    let egui_ctx = egui_ctx.clone();
    // Shared lens db (photo tier), same as the single-file path (`spawn_export`
    // in `export/mod.rs`) — so a batch item with an enabled, matched lens
    // correction in its persisted stack bakes + renders it identically to a
    // single-file export of the same image. `None` when no db is loaded.
    let lens_db = state.lens_db.clone();
    let handle = state.jobs.submit(Priority::Background, move |cancel| {
        run_batch_sequential(
            &items,
            cancel,
            |item| {
                // Announce the file now being written (output basename) for the
                // status-bar indicator.
                let name = item
                    .dest
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let _ = tx.send(AppEvent::ExportItemStarted { name });
                egui_ctx.request_repaint();

                crate::diag::export_item_begin();
                let t0 = std::time::Instant::now();
                let image_id = item.image_id;
                let mut last = 0u32;
                let mut progress = |done: u32, total: u32| {
                    // Throttle repaints like the single-file path (every 8 tiles
                    // + on completion) so progress advances without flooding.
                    let _ = tx.send(AppEvent::ExportProgress {
                        image_id,
                        done,
                        total,
                    });
                    if done == total || done.saturating_sub(last) >= 8 {
                        last = done;
                        egui_ctx.request_repaint();
                    }
                };
                let (ok, message) = run_one(
                    &gpu,
                    item,
                    working_space,
                    &options,
                    lens_db.as_ref(),
                    cancel,
                    &mut progress,
                );
                crate::diag::export_item_end(ok, t0.elapsed().as_millis() as u64);
                (ok, message)
            },
            |image_id, ok, message| {
                let _ = tx.send(AppEvent::BatchItemFinished {
                    image_id,
                    ok,
                    message,
                });
                egui_ctx.request_repaint();
            },
        );
    });
    vec![handle]
}

/// Drive the batch items strictly sequentially on the calling (worker) thread,
/// checking cancellation between items. `process` does the per-item work;
/// `finished` reports every item's `(image_id, ok, message)` outcome so the
/// aggregate progress count always completes even when cancelled. Pure
/// orchestration (no GPU/job coupling) so the ordering + cancel behavior is unit-
/// testable.
fn run_batch_sequential(
    items: &[BatchItem],
    cancel: &CancelToken,
    mut process: impl FnMut(&BatchItem) -> (bool, String),
    mut finished: impl FnMut(i64, bool, String),
) {
    for item in items {
        let (ok, message) = if cancel.is_cancelled() {
            (false, "Export cancelled".to_string())
        } else {
            process(item)
        };
        finished(item.image_id, ok, message);
    }
}

/// The edit document a batch item renders with: its persisted sidecar, or the
/// default (identity) document when there is no sidecar or it is malformed.
/// Mirrors `develop::thumb_regen::resolve_regen_stack`'s fallback shape for the
/// same reason: a missing/corrupt XMP sidecar must never fail or panic a batch
/// export — it just falls back to an unedited render for that one item.
///
/// Batch export previously hardcoded `OpStack::default()` here unconditionally,
/// justified by a comment reading "per-image edits are not persisted" — true
/// when written, stale once sidecars shipped. The effect was that editing 50
/// images and batch-exporting produced 50 UNEDITED files.
pub(crate) fn stack_for_item(path: &std::path::Path) -> OpStack {
    let xmp = ferrolite_catalog::sidecar_path(path);
    ferrolite_catalog::read_ops(&xmp)
        .and_then(|text| ferrolite_pipeline::deserialize(&text))
        .unwrap_or_default()
}

/// Decode + demosaic + **upright** one RAW for a batch export: the same
/// sensor-native-then-oriented sequence the viewer's full-decode path uses
/// (`viewer::load`'s `apply_orientation_linear` right after the demosaic).
///
/// The `apply_orientation_linear` call is load-bearing, not cosmetic. A
/// demosaic is sensor-native, but every consumer of the edit document works in
/// the ORIENTED frame: `Geometry::crop` is normalized against the image as
/// displayed (the crop overlay derives it from `ViewerState::image_dims`), the
/// library grid swaps dimensions for a rotating orientation, and the preview
/// and warm-prefetch paths both upright before rendering. Batch export was the
/// only `decode_full` call site that skipped it, so for any orientation that
/// swaps axes (`Rotate90`/`Rotate270`/`Transpose`/`Transverse`) it applied the
/// persisted crop to a transposed source: a portrait 367×551 crop of a
/// Rotate270 frame exported as a 551×367 landscape patch of an entirely
/// different region — indistinguishable, to the user, from "export lost my
/// crop". Rotation-free images were unaffected, which is why it went unnoticed.
fn decode_oriented_raw(path: &std::path::Path) -> Result<(LinearRgbaF32, ColorProfile), String> {
    let raw = ferrolite_decode::decode_full(path).map_err(|e| e.to_string())?;
    let profile = raw.color_profile.clone();
    let demosaiced = Rcd.to_linear_rgba_f32(&raw);
    Ok((
        ferrolite_decode::apply_orientation_linear(demosaiced, raw.orientation),
        profile,
    ))
}

fn run_one(
    gpu: &Arc<GpuContext>,
    item: &BatchItem,
    working_space: WorkingSpace,
    options: &ExportOptions,
    // Shared lens db (see `spawn_batch`'s comment); threaded through so this
    // item's persisted lens correction (if any) bakes exactly like the
    // single-file path.
    lens_db: Option<&Arc<LensfunDb>>,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(u32, u32),
) -> (bool, String) {
    if cancel.is_cancelled() {
        return (false, "Export cancelled".to_string());
    }
    // Decode full-res on the worker thread → (linear image, color profile).
    let (linear, profile) = match item.kind {
        FileKind::Raw => match decode_oriented_raw(&item.path) {
            Ok(pair) => pair,
            Err(e) => return (false, format!("Decode failed: {e}")),
        },
        _ => match ferrolite_decode::decode_preview(&item.path, item.kind) {
            Ok(buf) => (
                crate::viewer::load::preview_to_linear(&buf),
                ColorProfile::srgb_fallback(),
            ),
            Err(e) => return (false, format!("Decode failed: {e}")),
        },
    };
    if cancel.is_cancelled() {
        return (false, "Export cancelled".to_string());
    }
    // Read the persisted edit stack for THIS item (see `stack_for_item` doc) —
    // the fix for the "batch exports unedited" defect this module's doc comment
    // used to justify.
    let stack = stack_for_item(&item.path);
    // Match the on-screen path: dual-illuminant interpolation + normalize_neutral
    // (the demosaic already applied the as-shot WB gains, so the matrix must be
    // row-normalized or neutrals skew magenta). Use the PERSISTED stack's WB
    // temp (0.0 when the stack has no WB op, i.e. as-shot) — mirrors
    // `App::confirm_export`'s `self.camera_to_working(self.current_wb_temp())`
    // so a batch export of a non-zero-temp edit renders the same camera matrix
    // the user saw on screen, not the as-shot one.
    let temp = stack.white_balance().map(|w| w.temp).unwrap_or(0.0);
    let camera_to_working =
        crate::camera_matrix::wb_camera_to_working(&profile, temp, working_space);
    // Wrap in an `Arc` (cheap — no copy) so the just-decoded full-res buffer can
    // be reused below as the dehaze transmission source without re-decoding.
    let linear = Arc::new(linear);
    let pyramid = Arc::new(GpuPyramidSource::new(gpu, &linear));
    // Whole-image dehaze transmission source (design §5.3, ST-Task 5): needed
    // only when the persisted stack actually has dehaze active somewhere
    // (global op or a visible mask layer — `EditDoc::dehaze_active_anywhere`).
    // Batch has no cached preview-tier buffer (unlike `App::confirm_export`), so
    // it reuses the full-res `linear` it already decoded above; `render_tiled`
    // downscales it internally to `DEHAZE_MAX_TRANSMISSION_DIM` regardless, so
    // this is equivalent (and needs no extra decode).
    let transmission_source = stack.dehaze_active_anywhere().then(|| linear.clone());
    let req = ExportRequest {
        ctx: gpu,
        pyramid: &pyramid,
        stack: &stack,
        camera_to_working,
        working_space,
        // Bakes the persisted lens correction (if any) off-thread inside
        // `render_tiled`, exactly like the single-file export path — see
        // `spawn_batch`'s comment on `lens_db`.
        lens_db,
        options,
        dest: &item.dest,
        source_path: &item.path,
        // Whole-image dehaze atmospheric light (design §5.3): the batch export
        // always has the decoded CPU `linear` in scope (it built the pyramid
        // from it above), so it can estimate the real value here — no fallback
        // needed for this path.
        atmospheric_light: ferrolite_pipeline::estimate_atmospheric_light(&linear),
        transmission_source: transmission_source.as_deref(),
    };
    match run_export(req, cancel, progress) {
        Ok(outcome) => {
            let base = format!("Exported {}", outcome.dest.display());
            let msg = if outcome.warnings.is_empty() {
                base
            } else {
                format!("{base} ({})", outcome.warnings.join("; "))
            };
            (true, msg)
        }
        Err(ferrolite_export::ExportError::Cancelled) => (false, "Export cancelled".to_string()),
        Err(e) => (false, format!("Export failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique path in the OS temp dir for a `stack_for_item` test. A plain
    /// counter (not `{:?}` on a `ThreadId`, which produced invalid Windows
    /// filenames in earlier P7 tasks) plus the process id keeps concurrent test
    /// runs from colliding on the same sidecar path.
    fn unique_temp_path(label: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "ferrolite-batch-{label}-{}-{seq}.arw",
            std::process::id()
        ))
    }

    /// A fixture whose EXIF orientation SWAPS axes, or `None` when the
    /// git-ignored fixture set is absent (fresh clone / CI) — the established
    /// skip-when-no-fixtures pattern from `ferrolite-decode/tests/decode.rs`.
    /// `iso0200-…RW2` is recorded as **Rotate270** in `fixtures/raw/FIXTURES.md`
    /// and is the fixture the author's own crop test used.
    fn rotated_fixture() -> Option<std::path::PathBuf> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fixtures/raw/iso0200-panasonic-dc-s5-iso200-105mm.RW2");
        p.is_file().then_some(p)
    }

    /// Regression: batch export must hand `render_tiled` an UPRIGHT source, so a
    /// persisted crop (normalized against the oriented frame the user cropped
    /// on) lands on the region they chose. `decode_oriented_raw` was previously
    /// inlined in `run_one` WITHOUT `apply_orientation_linear` — the only one of
    /// the four `decode_full` call sites to skip it — which transposed every
    /// rotated image's crop. See `decode_oriented_raw`'s doc comment.
    ///
    /// Asserts the axes really are swapped relative to the sensor-native
    /// demosaic, which is the exact property the missing call provided; a
    /// dims-only check would pass even if the pixels were left unrotated.
    #[test]
    fn decode_oriented_raw_uprights_a_rotated_fixture() {
        let Some(path) = rotated_fixture() else {
            eprintln!("fixtures/raw absent; skipping");
            return;
        };
        let raw = ferrolite_decode::decode_full(&path).expect("decode_full");
        assert!(
            raw.orientation.swaps_dimensions(),
            "fixture must have a dimension-swapping orientation or this test is              vacuous, got {:?}",
            raw.orientation
        );
        let sensor_native = Rcd.to_linear_rgba_f32(&raw);
        let (oriented, _) = decode_oriented_raw(&path).expect("decode_oriented_raw");

        assert_eq!(
            (oriented.width, oriented.height),
            (sensor_native.height, sensor_native.width),
            "batch export must upright the demosaic (dims swapped) — without it              a persisted crop is applied to a transposed source and the export              shows an entirely different region"
        );
    }

    /// The extent a cropped batch export renders at must be derived from the
    /// UPRIGHT source. Pins the crop → output-extent contract for a rotated
    /// image: pre-fix this produced the transposed extent, i.e. a landscape file
    /// where the user cropped a portrait.
    #[test]
    fn a_cropped_rotated_export_renders_at_the_upright_crop_extent() {
        let Some(path) = rotated_fixture() else {
            eprintln!("fixtures/raw absent; skipping");
            return;
        };
        // A deliberately non-square normalized crop, so the upright and
        // transposed extents cannot coincide (the `assert_ne!` guard below).
        let stack = OpStack::default().set_op(ferrolite_pipeline::Op::Geometry(
            ferrolite_pipeline::Geometry {
                crop: ferrolite_pipeline::CropRect {
                    x: 0.1,
                    y: 0.2,
                    w: 0.5,
                    h: 0.25,
                },
                ..Default::default()
            },
        ));
        let raw = ferrolite_decode::decode_full(&path).expect("decode_full");
        let sensor_native = Rcd.to_linear_rgba_f32(&raw);
        // The frame the crop was authored against, derived from the RAW's own
        // orientation rather than from `decode_oriented_raw` — so the guard and
        // the expectation below stay independent of the code under test.
        let (uw, uh) = if raw.orientation.swaps_dimensions() {
            (sensor_native.height, sensor_native.width)
        } else {
            (sensor_native.width, sensor_native.height)
        };
        let expected = ferrolite_pipeline::edited_output_dims(&stack, uw, uh);
        let transposed = ferrolite_pipeline::edited_output_dims(
            &stack,
            sensor_native.width,
            sensor_native.height,
        );
        assert_ne!(
            expected, transposed,
            "this crop must distinguish the upright extent from the transposed              one, or the assertion below cannot fail"
        );

        let (oriented, _) = decode_oriented_raw(&path).expect("decode_oriented_raw");
        let got = ferrolite_pipeline::edited_output_dims(&stack, oriented.width, oriented.height);
        assert_eq!(
            got, expected,
            "a cropped batch export must render at the UPRIGHT crop extent;              {got:?} is the transposed extent, so the demosaic reached              `render_tiled` sensor-native"
        );
    }

    #[test]
    fn stack_for_item_is_default_when_no_sidecar_exists() {
        let p = unique_temp_path("no-sidecar");
        let xmp = ferrolite_catalog::sidecar_path(&p);
        let _ = std::fs::remove_file(&xmp); // guard against a stale leftover
        assert!(stack_for_item(&p).is_identity());
    }

    #[test]
    fn stack_for_item_reads_a_persisted_edit() {
        let p = unique_temp_path("with-sidecar");
        let xmp = ferrolite_catalog::sidecar_path(&p);
        let mut doc = OpStack::default();
        doc.global.exposure = 1.25;
        ferrolite_catalog::write_ops(&xmp, &ferrolite_pipeline::serialize(&doc)).unwrap();

        let got = stack_for_item(&p);
        assert_eq!(
            got.global.exposure, 1.25,
            "batch export must honour persisted edits"
        );

        let _ = std::fs::remove_file(&xmp);
    }

    #[test]
    fn stack_for_item_falls_back_to_default_on_a_malformed_sidecar() {
        let p = unique_temp_path("bad-sidecar");
        let xmp = ferrolite_catalog::sidecar_path(&p);
        ferrolite_catalog::write_ops(&xmp, "not json {{").unwrap();
        assert!(
            stack_for_item(&p).is_identity(),
            "malformed → default, never a panic"
        );
        let _ = std::fs::remove_file(&xmp);
    }

    #[test]
    fn persisted_wb_temp_is_extracted_for_the_camera_matrix() {
        // Regression for F1: batch export must use the PERSISTED stack's WB temp
        // (like `App::confirm_export`'s `self.camera_to_working(self.current_wb_temp())`),
        // not a hardcoded 0.0. This pins the exact expression `run_one` uses.
        let mut doc = OpStack::default();
        doc.global.temp = 2400.0;
        let temp = doc.white_balance().map(|w| w.temp).unwrap_or(0.0);
        assert_eq!(
            temp, 2400.0,
            "non-zero persisted WB temp must survive extraction"
        );

        let identity = OpStack::default();
        let identity_temp = identity.white_balance().map(|w| w.temp).unwrap_or(0.0);
        assert_eq!(
            identity_temp, 0.0,
            "identity stack still yields as-shot temp 0"
        );
    }

    fn item(id: i64) -> BatchItem {
        BatchItem {
            image_id: id,
            path: PathBuf::from(format!("in-{id}.raw")),
            kind: FileKind::Raw,
            dest: PathBuf::from(format!("out-{id}.avif")),
        }
    }

    #[test]
    fn processes_items_in_order_and_reports_each_once() {
        let items = [item(1), item(2), item(3)];
        let cancel = CancelToken::new();
        let mut order = Vec::new();
        let mut reported = Vec::new();
        run_batch_sequential(
            &items,
            &cancel,
            |it| {
                order.push(it.image_id);
                (true, format!("Exported {}", it.image_id))
            },
            |id, ok, _msg| reported.push((id, ok)),
        );
        assert_eq!(order, vec![1, 2, 3], "processed strictly in queue order");
        assert_eq!(
            reported,
            vec![(1, true), (2, true), (3, true)],
            "every item reported exactly once"
        );
    }

    #[test]
    fn already_cancelled_batch_processes_nothing_but_still_reports_all() {
        // Cancel before starting: no per-item GPU/encode work runs, yet every item
        // is still reported (ok=false) so the aggregate progress count completes
        // and the batch clears cleanly.
        let items = [item(10), item(11)];
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut processed = 0;
        let mut reported = Vec::new();
        run_batch_sequential(
            &items,
            &cancel,
            |_it| {
                processed += 1;
                (true, "unexpected".to_string())
            },
            |id, ok, msg| reported.push((id, ok, msg)),
        );
        assert_eq!(processed, 0, "cancelled batch does no per-item work");
        assert_eq!(reported.len(), 2, "all items still reported for the count");
        assert!(
            reported
                .iter()
                .all(|(_, ok, msg)| !ok && msg == "Export cancelled"),
            "cancelled items reported as failed with the cancel message"
        );
    }

    #[test]
    fn mid_batch_cancel_stops_processing_remaining_items() {
        // Cancelling during item 1 means items 2..n skip processing (no more heavy
        // work) but are still reported so the count reaches total.
        let items = [item(1), item(2), item(3)];
        let cancel = CancelToken::new();
        let mut processed = Vec::new();
        run_batch_sequential(
            &items,
            &cancel,
            |it| {
                processed.push(it.image_id);
                cancel.cancel(); // simulate cancel arriving during the first item
                (true, "ok".to_string())
            },
            |_id, _ok, _msg| {},
        );
        assert_eq!(
            processed,
            vec![1],
            "only the first item is processed; the rest are skipped after cancel"
        );
    }
}
