//! The Export module (spec §8.4): a third top-level module. Chrome grammar —
//! toolbar (queue summary + Clear) → content row (queue list · settings panel ·
//! bottom bar). This file owns the toolbar + the outbound action enum; the
//! content panels live in `queue_list` and `bottom_bar`.

use crate::state::AppState;

pub mod bottom_bar;
pub mod queue_list;
pub mod settings_panel;

pub use settings_panel::export_settings_panel;

/// Actions the Export module surfaces up to `app.rs` (which owns GPU state).
pub enum ExportModuleAction {
    /// The user hit Start with a chosen destination — run the batch.
    Start,
    /// Cancel the running batch.
    Cancel,
}

/// Width of the Size slider's allocation, matching the Library toolbar's.
const SIZE_SLIDER_W: f32 = 208.0_f32;

/// The 40px Export toolbar: queue count + "Clear queue" + the grid Size slider.
///
/// Returns `true` when the Size slider changed, so the caller can persist
/// `settings.export_grid_size` and mark settings dirty (the same shape as the
/// Library toolbar's own change reporting).
#[must_use]
pub fn toolbar(ui: &mut egui::Ui, state: &mut AppState) -> bool {
    let mut size_changed = false;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.label(format!(
            "Export queue — {} image(s)",
            state.export_queue.len()
        ));
        let running = state.batch_running();
        ui.add_enabled_ui(!state.export_queue.is_empty() && !running, |ui| {
            if ui.button("Clear queue").clicked() {
                state.queue_clear();
            }
        });

        // Grid Size slider, right-aligned exactly like the Library toolbar's,
        // and laid out the same way: "Size" as its own label so the slider's
        // label column collapses and the track keeps the freed width, with the
        // value readout and the per-control reset arrow in their own columns
        // (the reset comes from `EguiSlider`'s `default`).
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(SIZE_SLIDER_W, ui.available_height()),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(
                        egui::RichText::new("Size")
                            .size(11.0)
                            .color(crate::theme::TEXT_DIM),
                    );
                    let before = state.settings.export_grid_size;
                    ui.add(crate::widgets::slider::EguiSlider {
                        label: "",
                        value: &mut state.settings.export_grid_size,
                        min: 0.0_f32,
                        max: 100.0_f32,
                        default: 46.0_f32,
                        step: 1.0_f32,
                        decimals: 0,
                        unit: "",
                        bipolar: false,
                        signed: false,
                        custom_label_w: None,
                    });
                    if state.settings.export_grid_size != before {
                        size_changed = true;
                    }
                },
            );
        });
    });
    size_changed
}
