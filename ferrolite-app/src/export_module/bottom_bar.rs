//! Export module bottom bar (spec §8.4): destination folder + filename template
//! + Start/Cancel + aggregate progress.

use crate::export_module::ExportModuleAction;
use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) -> Option<ExportModuleAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        if ui.button("Destination folder…").clicked() {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                state.export_dest = Some(dir);
            }
        }
        match &state.export_dest {
            Some(d) => ui.monospace(d.display().to_string()),
            None => ui.colored_label(crate::theme::TEXT_FAINT, "(no folder chosen)"),
        };
    });
    ui.horizontal(|ui| {
        ui.label("Filename");
        ui.add(
            egui::TextEdit::singleline(&mut state.export_template)
                .hint_text("{name}")
                .desired_width(220.0),
        );
        if ui
            .small_button("?")
            .on_hover_text("Filename tokens")
            .clicked()
        {
            state.export_help_open = true;
        }
    });

    let mut help_open = state.export_help_open;
    egui::Window::new("Filename tokens")
        .collapsible(false)
        .resizable(false)
        .open(&mut help_open)
        .show(ui.ctx(), |ui| {
            egui::Grid::new("export_filename_tokens_grid")
                .num_columns(2)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    ui.monospace("{name}");
                    ui.label("Original file basename");
                    ui.end_row();

                    ui.monospace("{seq}");
                    ui.label("Sequence number (1, 2, 3, …)");
                    ui.end_row();

                    ui.monospace("{seq:03}");
                    ui.label("Zero-padded sequence (001, 002, …; any width N via {seq:0N})");
                    ui.end_row();

                    ui.monospace("{date}");
                    ui.label("Capture date (YYYY-MM-DD)");
                    ui.end_row();
                });
            ui.separator();
            ui.colored_label(
                crate::theme::TEXT_FAINT,
                "Any other text is kept literally.",
            );
        });
    state.export_help_open = help_open;
    ui.horizontal(|ui| {
        let running = state.batch.as_ref().is_some_and(|b| !b.is_done());
        let can_start = !running
            && !state.export_queue.is_empty()
            && state.export_dest.is_some()
            && !state.export_template.trim().is_empty();
        ui.add_enabled_ui(can_start, |ui| {
            if ui.button("Start export").clicked() {
                action = Some(ExportModuleAction::Start);
            }
        });
        if running && ui.button("Cancel").clicked() {
            action = Some(ExportModuleAction::Cancel);
        }
        if let Some(b) = state.batch.as_ref() {
            let msg = if b.is_done() {
                format!(
                    "Done — {} exported, {} failed",
                    b.completed - b.failed,
                    b.failed
                )
            } else {
                format!(
                    "Exporting {}/{} ({} failed)",
                    b.completed, b.total, b.failed
                )
            };
            ui.label(msg);
        }
    });
    action
}
