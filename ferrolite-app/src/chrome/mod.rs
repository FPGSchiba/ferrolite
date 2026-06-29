//! Custom window chrome: the borderless title bar, window controls, and app icon.
pub mod icon;
pub mod window_controls;

use crate::module::Module;
use crate::theme;
use egui::{vec2, Align, Context, Layout, Sense, UiBuilder};

/// Render the borderless title bar contents. `ui` is the 30px top panel's ui.
/// Left: icon + wordmark + menu labels. Center: Library/Develop tabs.
/// Right: version + window controls. Empty space drags the window.
pub fn title_bar(ctx: &Context, ui: &mut egui::Ui, module: &mut Module, version: &str) {
    let bar = ui.max_rect();

    // 1) Drag region over the whole bar (added first => lowest input priority;
    //    widgets drawn after take their clicks, empty space starts a window drag).
    let drag = ui.interact(bar, ui.id().with("titlebar_drag"), Sense::click_and_drag());
    if drag.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
    }

    // 2) Left cluster: icon mark + wordmark + menu labels.
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(bar)
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

    // 3) Center cluster: module tabs, horizontally centered in the bar.
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(bar)
            .layout(Layout::top_down(Align::Center)),
        |ui| {
            // top_down starts the cursor at bar.min.y; nudge down so the row is vertically centered.
            let row_h =
                ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().button_padding.y * 2.0;
            ui.add_space(((bar.height() - row_h) * 0.5).max(0.0));
            ui.horizontal(|ui| {
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
            });
        },
    );

    // 4) Right cluster: window controls (rightmost) then version.
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(bar)
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            if let Some(action) = window_controls::controls_ui(ui) {
                let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                ctx.send_viewport_cmd(window_controls::command(action, max));
            }
            ui.add_space(8.0);
            ui.monospace(version);
        },
    );
}
