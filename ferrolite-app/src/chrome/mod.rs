//! Custom window chrome: the borderless title bar, window controls, and app icon.
pub mod icon;
pub mod window_controls;

use crate::module::Module;
use crate::theme;
use egui::{
    pos2, vec2, Align, Align2, Context, FontId, Layout, PointerButton, Rect, Sense, UiBuilder,
};

/// Render the borderless title bar contents. `ui` is the 30px top panel's ui.
/// Left: icon + wordmark + menu labels (painted directly). Center: Library/Develop
/// tabs. Right: window controls + version. Empty space drags the window.
///
/// Layout mirrors eframe's `custom_window_frame` example: the window-drag region is
/// registered first (lowest input priority), the non-interactive left content is
/// PAINTED directly (so it never occludes the drag region), and only the interactive
/// groups (controls, tabs) use child UIs — they sit on top and win their own clicks.
pub fn title_bar(ctx: &Context, ui: &mut egui::Ui, module: &mut Module, version: &str) {
    let bar = ui.max_rect();

    // Window drag + double-click-to-maximize over the whole bar (registered first).
    let drag = ui.interact(bar, ui.id().with("titlebar_drag"), Sense::click_and_drag());
    if drag.drag_started_by(PointerButton::Primary) {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked_by(PointerButton::Primary) {
        let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
    }
    let is_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));

    // Left content: PAINTED directly (no child Ui), so it is non-interactive and never
    // occludes the full-bar drag region. Lay it out left-to-right by advancing `x`.
    {
        let painter = ui.painter();
        let cy = bar.center().y;
        let mut x = bar.left() + 8.0;
        icon::paint_mark(
            painter,
            Rect::from_min_size(pos2(x, cy - 9.0), vec2(18.0, 18.0)),
        );
        x += 24.0;
        let logo = painter.text(
            pos2(x, cy),
            Align2::LEFT_CENTER,
            "FERROLITE",
            FontId::proportional(11.0),
            theme::TEXT_PRIMARY,
        );
        x = logo.right() + 14.0;
        for m in ["File", "Edit", "Photo", "View", "Help"] {
            let r = painter.text(
                pos2(x, cy),
                Align2::LEFT_CENTER,
                m,
                FontId::proportional(11.5),
                theme::TEXT_DIM,
            );
            x = r.right() + 12.0;
        }
    }

    // Right group: window controls (close rightmost) + version, in a right-to-left
    // child Ui over the WHOLE bar. As in eframe, the empty left part of this Ui stays
    // draggable; only the buttons/version consume input.
    let control_clicked = ui
        .allocate_new_ui(
            UiBuilder::new()
                .max_rect(bar)
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let clicked = window_controls::controls_ui(ui, is_maximized);
                ui.add_space(8.0);
                ui.monospace(version);
                clicked
            },
        )
        .inner;
    if let Some(action) = control_clicked {
        ctx.send_viewport_cmd(window_controls::command(action, is_maximized));
    }

    // Center group: Library/Develop tabs in a content-sized rect at the bar centre
    // (drawn last so the tabs sit on top of the drag region).
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
