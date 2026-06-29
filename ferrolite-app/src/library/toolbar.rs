//! Library top toolbar: search (stub), sort (stub), and the thumbnail-size slider.

use crate::widgets::EguiSlider;

pub fn show(ui: &mut egui::Ui, thumb_size: &mut f32) {
    ui.horizontal(|ui| {
        ui.add_enabled(
            false,
            egui::TextEdit::singleline(&mut String::new()).hint_text("Search"),
        );
        ui.separator();
        ui.label("Sort:");
        ui.add_enabled(false, egui::Label::new("Filename"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
        });
    });
}
