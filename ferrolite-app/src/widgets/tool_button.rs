//! Shared Develop tool/tab button — consistent active/hover/disabled styling using
//! design-system tokens, so palette tools and panel tabs look identical everywhere.

use crate::theme;

/// A compact icon button. `active` → accent fill; `!enabled` → faint + a hover reason;
/// otherwise idle with a hover highlight. Returns the click `Response`.
pub(crate) fn tool_button(
    ui: &mut egui::Ui,
    icon: &str,
    tooltip: &str,
    active: bool,
    enabled: bool,
    disabled_reason: Option<&str>,
) -> egui::Response {
    let size = egui::vec2(28.0, 28.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = resp.hovered() && enabled;
    let bg = if active {
        theme::ACCENT_BRIGHT
    } else if hovered {
        theme::BG_TOOLBAR
    } else {
        egui::Color32::TRANSPARENT
    };
    let fg = if !enabled {
        theme::TEXT_FAINT
    } else if active {
        theme::BG_BASE
    } else {
        theme::TEXT_DIM
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, bg);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(15.0),
        fg,
    );
    if enabled {
        resp = resp.on_hover_text(tooltip);
    } else if let Some(reason) = disabled_reason {
        resp = resp.on_hover_text(reason);
    }
    resp
}
