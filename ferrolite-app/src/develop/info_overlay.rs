//! Compact read-only HUD of the key photographic facts + live zoom, drawn as a
//! hovering translucent-black box pinned to the image canvas — mirrors the
//! histogram overlay's style (`draw_histogram_overlay`), anchored bottom-left so
//! it never clashes with the top-right histogram. Consumes only a formatted
//! `ImageFacts`; `ui.min_rect()` is the canvas rect it pins against.

/// Facts-box background, matching the histogram overlay's translucent black.
const FILL_ALPHA: u8 = 160;
/// Inset from the canvas edges, matching the histogram overlay's margin.
const MARGIN: f32 = 12.0;
/// Bright near-white so text stays legible on the black box regardless of theme.
const TEXT: egui::Color32 = egui::Color32::from_gray(235);

pub fn draw(ui: &egui::Ui, facts: &crate::develop::info::ImageFacts) {
    let canvas_rect = ui.min_rect();
    // Bottom-left corner of the box, `MARGIN` inside the canvas. The pivot makes
    // this the box's lower-left corner, so the box grows upward as lines are
    // added without needing to know its height in advance.
    let pos = egui::pos2(canvas_rect.left() + MARGIN, canvas_rect.bottom() - MARGIN);

    egui::Area::new(egui::Id::new("develop_info_overlay"))
        .order(egui::Order::Middle)
        .fixed_pos(pos)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_black_alpha(FILL_ALPHA))
                .rounding(4.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    for line in [
                        &facts.focal,
                        &facts.aperture,
                        &facts.shutter,
                        &facts.iso,
                        &facts.zoom,
                    ] {
                        if !line.is_empty() {
                            ui.label(egui::RichText::new(line.as_str()).color(TEXT));
                        }
                    }
                });
        });
}
