pub mod chips;
pub mod color_wheel;
pub mod curve;
pub mod range_slider;
pub mod slider;
pub mod tabs;
pub mod tool_button;
#[allow(unused_imports)]
pub use chips::{multi_select_chips, segmented_control, SegmentedControl};
#[allow(unused_imports)]
pub use color_wheel::{color_grading_wheel, color_wheel, ColorGradingEdit, WheelEdit};
#[allow(unused_imports)]
pub use curve::{
    curve_editor, tone_curve_widget, CurveEdit, CurveStyle, ParametricCurveValues, ToneCurveEdit,
    ToneCurveTab,
};
#[allow(unused_imports)]
pub use range_slider::RangeSlider;
pub use slider::EguiSlider;
#[allow(unused_imports)]
pub use tabs::{tab_row, TabRow};
pub(crate) use tool_button::tool_button;

use egui::Color32;

/// Draw the per-control "reset" glyph (`icons::RESET`, a counter-clockwise
/// arrow) centered at `center`, sized to visually match the radius `r` the
/// old hand-built arc+arrowhead used.
///
/// Shared by `EguiSlider` and any other editable control that needs a
/// per-control reset affordance (see the design rule in the root `CLAUDE.md`).
pub(crate) fn draw_reset_arrow(
    painter: &egui::Painter,
    center: egui::Pos2,
    r: f32,
    color: Color32,
) {
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        crate::icons::RESET,
        crate::icons::font(r * 2.2), // tuned to match the reset column's prior visual size
        color,
    );
}

/// Collapsible section header row (design §6).
/// Font: 10px monospace (`plex-mono`), 1.0px letter-spacing, `#6a6a6a` (`theme::TEXT_FAINT`).
/// Chevron `icons::CARET_RIGHT` / `icons::CARET_DOWN` + title + 1px `#232323` bottom divider line.
/// Click toggles `*is_open = !*is_open` and marks response changed.
#[allow(dead_code)]
pub fn section_header(ui: &mut egui::Ui, label: &str, is_open: &mut bool) -> egui::Response {
    let full = ui.available_width();
    let row_h = 22.0;
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(full, row_h), egui::Sense::click());

    if response.clicked() {
        *is_open = !*is_open;
        response.mark_changed();
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let chevron = if *is_open {
            crate::icons::CARET_DOWN
        } else {
            crate::icons::CARET_RIGHT
        };

        let mut job = egui::text::LayoutJob::default();
        job.append(
            chevron,
            0.0,
            egui::TextFormat {
                font_id: crate::icons::font(10.0),
                color: crate::theme::TEXT_FAINT,
                ..Default::default()
            },
        );
        job.append(
            &format!(" {label}"),
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::monospace(10.0),
                color: crate::theme::TEXT_FAINT,
                extra_letter_spacing: 1.0,
                ..Default::default()
            },
        );

        let galley = painter.layout_job(job);
        let text_pos = egui::pos2(rect.left() + 4.0, rect.center().y - galley.size().y * 0.5);
        painter.galley(text_pos, galley, crate::theme::TEXT_FAINT);

        // 1px divider line (#232323) across the bottom edge
        let line_y = rect.bottom() - 0.5;
        let line_color = Color32::from_rgb(0x23, 0x23, 0x23);
        painter.line_segment(
            [
                egui::pos2(rect.left(), line_y),
                egui::pos2(rect.right(), line_y),
            ],
            egui::Stroke::new(1.0_f32, line_color),
        );
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Context, RawInput};

    #[test]
    fn test_section_header_toggle() {
        let ctx = Context::default();
        let mut is_open = false;

        // Render pass 1: non-clicked render
        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = section_header(ui, "BASIC TONE", &mut is_open);
                assert!(!resp.changed());
            });
        });
        assert!(!is_open);

        // Pass 2: pointer click on header
        let mut input = RawInput::default();
        let click_pos = egui::pos2(50.0, 10.0);
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });

        let mut changed = false;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = section_header(ui, "BASIC TONE", &mut is_open);
                changed = resp.changed();
            });
        });

        assert!(is_open);
        assert!(changed);
    }

    #[test]
    fn test_section_header_icon_rendering() {
        let ctx = Context::default();

        // Render closed state (renders CARET_RIGHT)
        let mut is_open_closed = false;
        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = section_header(ui, "CLOSED SECTION", &mut is_open_closed);
                assert!(!resp.changed());
            });
        });

        // Render open state (renders CARET_DOWN)
        let mut is_open_open = true;
        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = section_header(ui, "OPEN SECTION", &mut is_open_open);
                assert!(!resp.changed());
            });
        });
    }
}
