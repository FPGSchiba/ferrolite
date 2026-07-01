//! Batch export orchestration (spec §8.4). One `ferrolite-export` Background job
//! per queued image. Each job decodes its image ON THE WORKER THREAD (never the
//! UI thread), builds the GPU pyramid inside the job (reusing the single-file
//! `ExportSource::FullResCpu` rationale), computes camera→working from the decoded
//! ColorProfile, and renders with `OpStack::default()` — per-image edits are not
//! persisted, so batch export is color-managed but unedited (spec §2 non-goal).

use std::path::PathBuf;
use std::sync::Arc;

use ferrolite_color::WorkingSpace;
use ferrolite_decode::{ColorProfile, DemosaicToRgb16f, QuadBin};
use ferrolite_export::{run_export, ExportOptions, ExportRequest};
use ferrolite_gpu::GpuContext;
use ferrolite_image::FileKind;
use ferrolite_jobs::{CancelToken, JobHandle, Priority};
use ferrolite_pipeline::{GpuPyramidSource, OpStack};

use crate::events::AppEvent;
use crate::state::AppState;

/// One image to export in a batch. `dest` is the final, collision-resolved path.
#[derive(Debug, Clone)]
// Task 7 (queue list + Start button) constructs and consumes these; until then
// the fields are only exercised by `spawn_batch`, which is itself dead code.
#[allow(dead_code)]
pub struct BatchItem {
    pub image_id: i64,
    pub path: PathBuf,
    pub kind: FileKind,
    pub dest: PathBuf,
}

/// Aggregate progress + cancellation handles for a running batch.
#[derive(Default)]
pub struct BatchExportState {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub handles: Vec<JobHandle>,
    pub warnings: Vec<String>,
}

impl BatchExportState {
    // Task 7 (queue list + Start button) is the first caller; remove this allow then.
    #[allow(dead_code)]
    pub fn new(total: usize) -> Self {
        Self {
            total,
            ..Default::default()
        }
    }
    pub fn is_done(&self) -> bool {
        self.completed >= self.total
    }
    // Task 7 wires the Cancel button that calls this; remove this allow then.
    #[allow(dead_code)]
    pub fn cancel_all(&self) {
        for h in &self.handles {
            h.cancel();
        }
    }
}

/// Submit one Background job per item. Returns the job handles (for cancellation).
// Task 7 (queue list + Start button) is the first caller; remove this allow then.
#[allow(dead_code)]
pub fn spawn_batch(
    state: &AppState,
    egui_ctx: &egui::Context,
    gpu: Arc<GpuContext>,
    items: Vec<BatchItem>,
    working_space: WorkingSpace,
    options: ExportOptions,
) -> Vec<JobHandle> {
    let mut handles = Vec::with_capacity(items.len());
    for item in items {
        let tx = state.tx.clone();
        let egui_ctx = egui_ctx.clone();
        let gpu = Arc::clone(&gpu);
        let handle = state.jobs.submit(Priority::Background, move |cancel| {
            let (ok, message) = run_one(&gpu, &item, working_space, &options, cancel);
            let _ = tx.send(AppEvent::BatchItemFinished {
                image_id: item.image_id,
                ok,
                message,
            });
            egui_ctx.request_repaint();
        });
        handles.push(handle);
    }
    handles
}

fn run_one(
    gpu: &Arc<GpuContext>,
    item: &BatchItem,
    working_space: WorkingSpace,
    options: &ExportOptions,
    cancel: &CancelToken,
) -> (bool, String) {
    if cancel.is_cancelled() {
        return (false, "Export cancelled".to_string());
    }
    // Decode full-res on the worker thread → (linear image, color profile).
    let (linear, profile) = match item.kind {
        FileKind::Raw => match ferrolite_decode::decode_full(&item.path) {
            Ok(raw) => {
                let profile = raw.color_profile.clone();
                (QuadBin.to_linear_rgba_f32(&raw), profile)
            }
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
    let camera_to_working = ferrolite_color::camera_to_working(
        profile.xyz_to_cam,
        ferrolite_color::Xy {
            x: profile.white_xy[0],
            y: profile.white_xy[1],
        },
        working_space,
    );
    let pyramid = Arc::new(GpuPyramidSource::new(gpu, &linear));
    let stack = OpStack::default();
    let mut noop = |_done: u32, _total: u32| {};
    let req = ExportRequest {
        ctx: gpu,
        pyramid: &pyramid,
        stack: &stack,
        camera_to_working,
        working_space,
        options,
        dest: &item.dest,
        source_path: &item.path,
    };
    match run_export(req, cancel, &mut noop) {
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
