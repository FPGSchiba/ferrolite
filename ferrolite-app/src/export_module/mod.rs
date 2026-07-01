//! The Export module (spec §8.4): a third top-level module. Chrome grammar —
//! toolbar (queue summary + Clear) → content row (queue list · settings panel ·
//! bottom bar). Panels are filled in Task 7; this file owns the toolbar + the
//! outbound action enum.

use crate::state::AppState;

/// Actions the Export module surfaces up to `app.rs` (which owns GPU state).
#[allow(dead_code)] // constructed by queue_list/bottom_bar, handled in app.rs (Task 7)
pub enum ExportModuleAction {
    /// The user hit Start with a chosen destination — run the batch.
    Start,
    /// Cancel the running batch.
    Cancel,
}

/// The 40px Export toolbar: queue count + "Clear queue".
pub fn toolbar(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.label(format!(
            "Export queue — {} image(s)",
            state.export_queue.len()
        ));
        let running = state.batch.as_ref().is_some_and(|b| !b.is_done());
        ui.add_enabled_ui(!state.export_queue.is_empty() && !running, |ui| {
            if ui.button("Clear queue").clicked() {
                state.queue_clear();
            }
        });
    });
}
