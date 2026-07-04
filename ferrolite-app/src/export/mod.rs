//! Single-file Photo → Export flow (spec §8.3): a format+options popup, then an
//! rfd destination picker, then one ferrolite-jobs Background export job.

use std::path::PathBuf;
use std::sync::Arc;

use ferrolite_color::WorkingSpace;
use ferrolite_export::{run_export, ExportOptions, ExportRequest};
use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_jobs::Priority;
use ferrolite_pipeline::GpuPyramidSource;

use crate::events::AppEvent;
use crate::state::AppState;

pub mod activity;
pub mod batch;
pub mod settings_form;

pub use activity::{ExportActivity, ExportKind};
use settings_form::settings_form;

/// The full-resolution source an export renders from. RAW images have a
/// GPU-resident pyramid (tier-2); Standard (JPEG/PNG/…) images are never tier-2
/// decoded — their tier-1 preview already IS the full-resolution image, so the
/// pyramid is built from that CPU buffer inside the Background job (off the UI
/// thread) via the shared device.
pub enum ExportSource {
    /// A ready GPU pyramid (RAW). Used directly.
    Pyramid(Arc<GpuPyramidSource>),
    /// A full-res CPU image (Standard). The job builds the pyramid from it.
    FullResCpu(Arc<LinearRgbaF32>),
}

#[derive(Default)]
pub struct ExportDialogState {
    pub options: ExportOptions,
}

pub enum DialogOutcome {
    Confirm,
    Cancel,
}

/// Draw the export options popup. Returns `Some(Confirm)` when the user hits
/// "Choose destination…", `Some(Cancel)` on cancel/close, else `None`.
pub fn draw_dialog(ctx: &egui::Context, dialog: &mut ExportDialogState) -> Option<DialogOutcome> {
    let mut outcome = None;
    let mut open = true;
    egui::Window::new("Export")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            settings_form(ui, &mut dialog.options);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Choose destination…").clicked() {
                    outcome = Some(DialogOutcome::Confirm);
                }
                if ui.button("Cancel").clicked() {
                    outcome = Some(DialogOutcome::Cancel);
                }
            });
        });
    if !open && outcome.is_none() {
        outcome = Some(DialogOutcome::Cancel);
    }
    outcome
}

/// Submit ONE Background export job for the currently open image. Captures the
/// shared GpuContext + resident pyramid; builds the TileEditPipeline inside the
/// closure (worker thread). Progress + completion flow back over the app channel.
#[allow(clippy::too_many_arguments)]
pub fn spawn_export(
    state: &AppState,
    egui_ctx: &egui::Context,
    gpu: Arc<GpuContext>,
    source: ExportSource,
    stack: ferrolite_pipeline::OpStack,
    camera_to_working: [[f32; 3]; 3],
    working_space: WorkingSpace,
    options: ExportOptions,
    source_path: PathBuf,
    dest: PathBuf,
    image_id: i64,
) -> ferrolite_jobs::JobHandle {
    let tx = state.tx.clone();
    let egui_ctx = egui_ctx.clone();
    // Shared lens db (photo tier) so the export bakes + renders any enabled lens
    // correction off-thread inside the job. `None` when no db is loaded.
    let lens_db = state.lens_db.clone();
    let handle = state.jobs.submit(Priority::Background, move |cancel| {
        // Resolve the source to a GPU pyramid. For a Standard image this uploads
        // the full-res pyramid on the worker thread (never the UI thread).
        let pyramid = match source {
            ExportSource::Pyramid(p) => p,
            ExportSource::FullResCpu(img) => Arc::new(GpuPyramidSource::new(&gpu, &img)),
        };
        let mut last_repaint = 0u32;
        let mut progress = |done: u32, total: u32| {
            let _ = tx.send(AppEvent::ExportProgress {
                image_id,
                done,
                total,
            });
            // Repaint occasionally so the status bar advances without flooding.
            if done == total || done.saturating_sub(last_repaint) >= 8 {
                last_repaint = done;
                egui_ctx.request_repaint();
            }
        };
        let req = ExportRequest {
            ctx: &gpu,
            pyramid: &pyramid,
            stack: &stack,
            camera_to_working,
            working_space,
            lens_db: lens_db.as_ref(),
            options: &options,
            dest: &dest,
            source_path: &source_path,
        };
        let (ok, message) = match run_export(req, cancel, &mut progress) {
            Ok(outcome) => {
                let base = format!("Exported to {}", outcome.dest.display());
                let msg = if outcome.warnings.is_empty() {
                    base
                } else {
                    format!("{base} ({})", outcome.warnings.join("; "))
                };
                (true, msg)
            }
            Err(ferrolite_export::ExportError::Cancelled) => {
                (false, "Export cancelled".to_string())
            }
            Err(e) => (false, format!("Export failed: {e}")),
        };
        let _ = tx.send(AppEvent::ExportFinished {
            image_id,
            ok,
            message,
        });
        egui_ctx.request_repaint();
    });
    handle
}
