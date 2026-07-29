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
/// Approximate height of the info overlay HUD box produced by `draw()`.
/// Used to position the toggle pill above the overlay when visible.
const OVERLAY_APPROX_H: f32 = 108.0;

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

/// Y-offset (up from the canvas bottom edge) of the info pill's anchor.
/// Overlay visible: sit above the overlay box. Hidden: sit at the corner margin.
pub(crate) fn pill_bottom_offset(overlay_visible: bool, overlay_height: f32) -> f32 {
    if overlay_visible {
        overlay_height + 2.0 * MARGIN
    } else {
        MARGIN
    }
}

/// Draw the floating `ℹ Info` pill button on the canvas. When the overlay is
/// hidden, anchors at the bottom-left corner margin. When visible, positions
/// above the EXIF overlay. Highlights with accent tint when `show_info_panel`
/// is true, and clicking toggles `show_info_panel`.
pub fn draw_toggle_button(ui: &egui::Ui, show_info_panel: &mut bool) {
    let canvas_rect = ui.min_rect();
    let offset = pill_bottom_offset(*show_info_panel, OVERLAY_APPROX_H);
    let pos = egui::pos2(canvas_rect.left() + MARGIN, canvas_rect.bottom() - offset);

    egui::Area::new(egui::Id::new("develop_info_pill_button"))
        .order(egui::Order::Middle)
        .fixed_pos(pos)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .show(ui.ctx(), |ui| {
            let (bg_color, stroke_color, text_color) = if *show_info_panel {
                (
                    crate::theme::ACCENT_BG_SEL,
                    egui::Stroke::new(1.0_f32, crate::theme::ACCENT),
                    crate::theme::ACCENT_BRIGHT,
                )
            } else {
                (
                    egui::Color32::from_black_alpha(FILL_ALPHA),
                    egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(30)),
                    TEXT,
                )
            };

            let btn = egui::Button::new(
                egui::RichText::new(format!("{} Info", crate::icons::INFO)).color(text_color),
            )
            .fill(bg_color)
            .stroke(stroke_color)
            .rounding(12.0);

            if ui.add(btn).clicked() {
                *show_info_panel = !*show_info_panel;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pill_docks_to_corner_when_overlay_hidden() {
        assert_eq!(pill_bottom_offset(false, 120.0), MARGIN);
        assert!(pill_bottom_offset(true, 120.0) > 120.0);
    }
}
