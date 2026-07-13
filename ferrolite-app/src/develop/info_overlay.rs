//! Compact read-only HUD of the key photographic facts + live zoom, toggled
//! like the histogram. Consumes only a formatted `ImageFacts`.

pub fn draw(ctx: &egui::Context, facts: &crate::develop::info::ImageFacts) {
    egui::Area::new("info_overlay".into())
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 12.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                for line in [
                    &facts.focal,
                    &facts.aperture,
                    &facts.shutter,
                    &facts.iso,
                    &facts.zoom,
                ] {
                    if !line.is_empty() {
                        ui.label(line.as_str());
                    }
                }
            });
        });
}
