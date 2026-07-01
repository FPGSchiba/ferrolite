//! The Export queue list (spec §8.4): filename rows with reorder + remove.
//! Read-only image metadata is fetched via the read pool by id.

use crate::state::AppState;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    if state.export_queue.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(
                egui::RichText::new("Queue is empty.\nAdd images from Library or Develop.")
                    .color(crate::theme::TEXT_FAINT),
            );
        });
        return;
    }
    // Resolve filenames for display (id → basename). Missing ids show the id.
    let ids = state.export_queue.clone();
    let recs = state.reads.images_by_ids(&ids).unwrap_or_default();
    let name_of = |id: i64| -> String {
        recs.iter()
            .find(|r| r.id == id)
            .map(|r| r.filename.clone())
            .unwrap_or_else(|| format!("#{id}"))
    };

    let running = state.batch.as_ref().is_some_and(|b| !b.is_done());
    let mut do_move: Option<(usize, isize)> = None;
    let mut do_remove: Option<i64> = None;

    egui::ScrollArea::vertical().show(ui, |ui| {
        for (idx, &id) in ids.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("{:>3}.", idx + 1));
                ui.label(name_of(id));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(!running, |ui| {
                        if ui.small_button("✕").clicked() {
                            do_remove = Some(id);
                        }
                        if ui.small_button("▼").clicked() {
                            do_move = Some((idx, 1));
                        }
                        if ui.small_button("▲").clicked() {
                            do_move = Some((idx, -1));
                        }
                    });
                });
            });
        }
    });

    if let Some((idx, delta)) = do_move {
        state.queue_move(idx, delta);
    }
    if let Some(id) = do_remove {
        state.queue_remove(id);
    }
}
