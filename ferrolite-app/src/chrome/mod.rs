//! Custom window chrome: the borderless title bar, window controls, and app icon.
pub mod icon;
pub mod window_controls;

use crate::module::Module;
use crate::theme;
use egui::{pos2, vec2, Align, Context, Layout, PointerButton, Rect, Sense, UiBuilder};

/// Render the borderless title bar contents. `ui` is the 30px top panel's ui.
/// Left: icon + wordmark + menu labels. Center: Library/Develop tabs.
/// Right: version + window controls. Empty space drags the window.
///
/// The three clusters live in their own bounded, non-overlapping rects so the
/// centered tabs don't left-align and the right side stays draggable; the
/// drag region is registered first (lowest input priority) so the controls and
/// tabs added afterwards win their own clicks.
pub fn title_bar(ctx: &Context, ui: &mut egui::Ui, module: &mut Module, version: &str) {
    let bar = ui.max_rect();

    // Window drag + double-click-to-maximize over the whole bar. Registered first
    // so it has the lowest input priority; interactive widgets added afterwards win
    // their own clicks, and only empty bar space starts a window drag.
    let drag = ui.interact(bar, ui.id().with("titlebar_drag"), Sense::click_and_drag());
    if drag.drag_started_by(PointerButton::Primary) {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked_by(PointerButton::Primary) {
        let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
    }

    // Right cluster: window controls (close rightmost) + version, in a bounded
    // right-anchored rect. Drawn after the drag region so the buttons receive clicks.
    let right_w = 3.0 * window_controls::BTN_W + 72.0;
    let right_rect = Rect::from_min_max(pos2(bar.right() - right_w, bar.top()), bar.right_bottom());
    let control_clicked = ui
        .allocate_new_ui(
            UiBuilder::new()
                .max_rect(right_rect)
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                let clicked = window_controls::controls_ui(ui);
                ui.add_space(8.0);
                ui.monospace(version);
                clicked
            },
        )
        .inner;
    if let Some(action) = control_clicked {
        let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(window_controls::command(action, max));
    }

    // Left cluster: icon mark + wordmark + menu labels, bounded so it can't reach
    // the controls region.
    let left_rect = Rect::from_min_max(bar.left_top(), pos2(bar.right() - right_w, bar.bottom()));
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(left_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.add_space(8.0);
            let (mark, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
            icon::paint_mark(ui.painter(), mark);
            ui.add_space(6.0);
            ui.label("FERROLITE");
            ui.add_space(12.0);
            for m in ["File", "Edit", "Photo", "View", "Help"] {
                ui.colored_label(theme::TEXT_DIM, m);
            }
        },
    );

    // Center cluster: Library/Develop tabs, centered in the whole bar using a
    // content-sized rect placed at the bar centre. Drawn last so the tabs sit on top.
    let font = egui::TextStyle::Button.resolve(ui.style());
    let text_w = |t: &str| {
        ui.fonts(|f| {
            f.layout_no_wrap(t.to_owned(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        })
    };
    let btn_pad = ui.spacing().button_padding.x * 2.0;
    let tabs_w =
        text_w("Library") + text_w("Develop") + btn_pad * 2.0 + ui.spacing().item_spacing.x;
    let center_rect = Rect::from_center_size(bar.center(), vec2(tabs_w, bar.height()));
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(center_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            if ui
                .selectable_label(module.is_library(), "Library")
                .clicked()
            {
                *module = Module::Library;
            }
            if ui
                .selectable_label(!module.is_library(), "Develop")
                .clicked()
            {
                *module = Module::Develop;
            }
        },
    );
}
