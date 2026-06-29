//! Library top toolbar, laid out to the design mockup: a fixed-width search
//! field, sort, and filter affordances on the left, and the thumbnail-size
//! slider pinned to the right at a fixed width (so it no longer stretches the
//! whole bar). Search/sort/filter are non-interactive visual stubs for Spec 1;
//! they are wired in a later phase.

use crate::widgets::EguiSlider;

/// Width of the thumbnail-size slider's box on the right. Fixed so the slider
/// (whose track otherwise expands to `available_width`) stays compact.
const SIZE_SLIDER_W: f32 = 208.0;

fn dim(text: &str) -> egui::RichText {
    egui::RichText::new(text)
        .color(crate::theme::TEXT_DIM)
        .size(11.0)
}

/// Returns true if the include-subfolders toggle changed this frame.
pub fn show(ui: &mut egui::Ui, thumb_size: &mut f32, include_subfolders: &mut bool) -> bool {
    let mut changed = false;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;

        let mut query = String::new();
        ui.add_enabled(
            false,
            egui::TextEdit::singleline(&mut query)
                .hint_text("Search catalog…")
                .desired_width(206.0),
        );

        ui.label(dim("Sort"));
        ui.add_enabled(false, egui::Button::new("Capture Time  ▾"));

        ui.label(dim("Filter"));
        ui.add_enabled(
            false,
            egui::Button::new(egui::RichText::new("★★★★★").color(crate::theme::TEXT_FAINT)),
        );
        ui.add_enabled(false, egui::Button::new("Metadata  ▾"));

        // Real toggle: include images from subfolders in the grid.
        changed = ui.checkbox(include_subfolders, "Subfolders").changed();

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(SIZE_SLIDER_W, ui.available_height()),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add(EguiSlider {
                        label: "Size",
                        value: thumb_size,
                        min: 0.0,
                        max: 100.0,
                        default: 46.0,
                        step: 1.0,
                        decimals: 0,
                        unit: "",
                        bipolar: false,
                        signed: false,
                    });
                },
            );
        });
    });
    changed
}
