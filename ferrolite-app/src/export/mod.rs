//! Single-file Photo → Export flow (spec §8.3): a format+options popup, then an
//! rfd destination picker, then one ferrolite-jobs Background export job.

use std::path::PathBuf;
use std::sync::Arc;

use ferrolite_color::WorkingSpace;
use ferrolite_export::{
    run_export, BitDepth, ExportFormat, ExportOptions, ExportRequest, ResizeSpec,
};
use ferrolite_gpu::GpuContext;
use ferrolite_jobs::Priority;
use ferrolite_pipeline::GpuPyramidSource;

use crate::events::AppEvent;
use crate::state::AppState;

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
            let o = &mut dialog.options;

            egui::ComboBox::from_label("Format")
                .selected_text(o.format.label())
                .show_ui(ui, |ui| {
                    for f in ExportFormat::ALL {
                        ui.selectable_value(&mut o.format, f, f.label());
                    }
                });

            egui::ComboBox::from_label("Output color space")
                .selected_text(format!("{:?}", o.output_space))
                .show_ui(ui, |ui| {
                    for ws in WorkingSpace::ALL {
                        ui.selectable_value(&mut o.output_space, ws, format!("{ws:?}"));
                    }
                });

            // Bit depth — 16-bit only for TIFF/PNG.
            ui.horizontal(|ui| {
                ui.label("Bit depth");
                ui.selectable_value(&mut o.bit_depth, BitDepth::Eight, "8-bit");
                ui.add_enabled_ui(o.format.supports_16bit(), |ui| {
                    ui.selectable_value(&mut o.bit_depth, BitDepth::Sixteen, "16-bit");
                });
            });
            if !o.format.supports_16bit() {
                o.bit_depth = BitDepth::Eight;
            }

            // Quality — JPEG only.
            ui.add_enabled_ui(o.format.supports_quality(), |ui| {
                ui.add(egui::Slider::new(&mut o.quality, 1..=100).text("Quality"));
            });

            // Resize.
            let mut mode = match o.resize {
                ResizeSpec::None => 0,
                ResizeSpec::LongEdge(_) => 1,
                ResizeSpec::Exact { .. } => 2,
                ResizeSpec::Percent(_) => 3,
            };
            egui::ComboBox::from_label("Resize")
                .selected_text(["None", "Long edge", "Exact", "Percent"][mode])
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut mode, 0, "None");
                    ui.selectable_value(&mut mode, 1, "Long edge");
                    ui.selectable_value(&mut mode, 2, "Exact");
                    ui.selectable_value(&mut mode, 3, "Percent");
                });
            o.resize = match mode {
                1 => {
                    let mut px = if let ResizeSpec::LongEdge(p) = o.resize {
                        p
                    } else {
                        2048
                    };
                    ui.add(
                        egui::DragValue::new(&mut px)
                            .range(1..=100_000)
                            .prefix("px "),
                    );
                    ResizeSpec::LongEdge(px)
                }
                2 => {
                    let (mut w, mut h) = if let ResizeSpec::Exact { w, h } = o.resize {
                        (w, h)
                    } else {
                        (1920, 1080)
                    };
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut w).range(1..=100_000).prefix("W "));
                        ui.add(egui::DragValue::new(&mut h).range(1..=100_000).prefix("H "));
                    });
                    ResizeSpec::Exact { w, h }
                }
                3 => {
                    let mut pct = if let ResizeSpec::Percent(p) = o.resize {
                        p * 100.0
                    } else {
                        50.0
                    };
                    ui.add(egui::Slider::new(&mut pct, 1.0..=100.0).suffix("%"));
                    ResizeSpec::Percent(pct / 100.0)
                }
                _ => ResizeSpec::None,
            };

            ui.separator();
            ui.checkbox(&mut o.copy_exif, "Copy EXIF metadata");
            ui.checkbox(&mut o.embed_icc, "Embed ICC profile");
            ui.checkbox(&mut o.strip_metadata, "Strip metadata");

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
    pyramid: Arc<GpuPyramidSource>,
    stack: ferrolite_pipeline::OpStack,
    camera_to_working: [[f32; 3]; 3],
    working_space: WorkingSpace,
    options: ExportOptions,
    source_path: PathBuf,
    dest: PathBuf,
    image_id: i64,
) {
    let tx = state.tx.clone();
    let egui_ctx = egui_ctx.clone();
    state.jobs.submit(Priority::Background, move |cancel| {
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
}
