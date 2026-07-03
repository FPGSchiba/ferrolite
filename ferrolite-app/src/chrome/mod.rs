//! Custom window chrome: the borderless title bar, window controls, and app icon.
pub mod icon;
pub mod window_controls;

use crate::module::Module;
use crate::settings::keymap::{Action, Keymap};
use crate::theme;
use egui::{
    pos2, vec2, Align, Align2, Button, Context, FontId, Layout, PointerButton, Rect, Sense,
    UiBuilder,
};

/// A menu action selected from the title-bar menus, handled by the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    ExportImage,
    AddToQueue,
    PurgePreviews,
    Exit,
    Undo,
    Redo,
    SelectAll,
    PrevImage,
    NextImage,
    SwitchModule(Module),
    ToggleSplit,
    ZoomFit,
    ZoomActual,
    ToggleHistogram,
    OpenHelp,
}

/// Build a menu `Button` labeled `text`, with the bound shortcut for `action`
/// shown right-aligned (Task 4.2). Enabled state is `enabled`.
fn menu_button(
    ui: &mut egui::Ui,
    keymap: &Keymap,
    text: &str,
    action: Action,
    enabled: bool,
) -> egui::Response {
    let btn = Button::new(text).shortcut_text(keymap.chord(action).label());
    ui.add_enabled(enabled, btn)
}

/// Render the borderless title bar contents. `ui` is the 30px top panel's ui.
/// Left: icon + wordmark + interactive menu row. Center: Library/Develop
/// tabs. Right: window controls + version. Empty space drags the window.
///
/// Layout mirrors eframe's `custom_window_frame` example: the window-drag region is
/// registered first (lowest input priority), the non-interactive left content
/// (icon + wordmark) is PAINTED directly (so it never occludes the drag region),
/// and only the interactive groups (menus, controls, tabs) use child UIs — they
/// sit on top and win their own clicks.
#[allow(clippy::too_many_arguments)]
pub fn title_bar(
    ctx: &Context,
    ui: &mut egui::Ui,
    module: &mut Module,
    version: &str,
    export_enabled: bool,
    viewer_open: bool,
    keymap: &Keymap,
    can_undo: bool,
    can_redo: bool,
    show_histogram: bool,
) -> Option<MenuAction> {
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
    let x = {
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
        logo.right() + 14.0
    };

    // Interactive menu row (on top of the drag region, like the tabs). Only
    // "Photo" is functional in this plan; the others are inert placeholders.
    let mut action: Option<MenuAction> = None;
    let menu_rect = Rect::from_min_max(
        pos2(x, bar.top()),
        pos2(bar.center().x - 60.0, bar.bottom()),
    );
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(menu_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            // Frameless, dim menu buttons to match the old painted look.
            ui.visuals_mut().widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
            ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::NONE;
            ui.menu_button("File", |ui| {
                if ui.button("Purge preview cache").clicked() {
                    action = Some(MenuAction::PurgePreviews);
                    ui.close_menu();
                }
                if ui.button("Exit").clicked() {
                    action = Some(MenuAction::Exit);
                    ui.close_menu();
                }
            });
            ui.menu_button("Edit", |ui| {
                if menu_button(ui, keymap, "Undo", Action::Undo, can_undo).clicked() {
                    action = Some(MenuAction::Undo);
                    ui.close_menu();
                }
                if menu_button(ui, keymap, "Redo", Action::Redo, can_redo).clicked() {
                    action = Some(MenuAction::Redo);
                    ui.close_menu();
                }
                if menu_button(ui, keymap, "Select all", Action::SelectAll, true).clicked() {
                    action = Some(MenuAction::SelectAll);
                    ui.close_menu();
                }
            });
            ui.menu_button("Photo", |ui| {
                if ui
                    .add_enabled(export_enabled, egui::Button::new("Export…"))
                    .clicked()
                {
                    action = Some(MenuAction::ExportImage);
                    ui.close_menu();
                }
                if menu_button(
                    ui,
                    keymap,
                    "Add to export queue",
                    Action::AddToQueue,
                    viewer_open,
                )
                .clicked()
                {
                    action = Some(MenuAction::AddToQueue);
                    ui.close_menu();
                }
                if menu_button(ui, keymap, "Previous image", Action::PrevImage, viewer_open)
                    .clicked()
                {
                    action = Some(MenuAction::PrevImage);
                    ui.close_menu();
                }
                if menu_button(ui, keymap, "Next image", Action::NextImage, viewer_open).clicked() {
                    action = Some(MenuAction::NextImage);
                    ui.close_menu();
                }
            });
            ui.menu_button("View", |ui| {
                if ui
                    .selectable_label(*module == Module::Library, "Library")
                    .clicked()
                {
                    action = Some(MenuAction::SwitchModule(Module::Library));
                    ui.close_menu();
                }
                if ui
                    .selectable_label(*module == Module::Develop, "Develop")
                    .clicked()
                {
                    action = Some(MenuAction::SwitchModule(Module::Develop));
                    ui.close_menu();
                }
                if ui
                    .selectable_label(*module == Module::Export, "Export")
                    .clicked()
                {
                    action = Some(MenuAction::SwitchModule(Module::Export));
                    ui.close_menu();
                }
                ui.separator();
                if menu_button(
                    ui,
                    keymap,
                    "Before/After split",
                    Action::ToggleSplitCompare,
                    viewer_open,
                )
                .clicked()
                {
                    action = Some(MenuAction::ToggleSplit);
                    ui.close_menu();
                }
                if ui.add_enabled(viewer_open, Button::new("Fit")).clicked() {
                    action = Some(MenuAction::ZoomFit);
                    ui.close_menu();
                }
                if ui.add_enabled(viewer_open, Button::new("1:1")).clicked() {
                    action = Some(MenuAction::ZoomActual);
                    ui.close_menu();
                }
                ui.separator();
                let mut histogram_checked = show_histogram;
                if ui
                    .checkbox(&mut histogram_checked, "Show histogram")
                    .clicked()
                {
                    action = Some(MenuAction::ToggleHistogram);
                    ui.close_menu();
                }
            });
            ui.menu_button("Help", |ui| {
                if menu_button(ui, keymap, "Keyboard shortcuts", Action::OpenHelp, true).clicked() {
                    action = Some(MenuAction::OpenHelp);
                    ui.close_menu();
                }
                if ui.button("About Ferrolite").clicked() {
                    action = Some(MenuAction::OpenHelp);
                    ui.close_menu();
                }
            });
        },
    );

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
    let tabs_w = text_w("Library")
        + text_w("Develop")
        + text_w("Export")
        + btn_pad * 3.0
        + ui.spacing().item_spacing.x * 2.0;
    let center_rect = Rect::from_center_size(bar.center(), vec2(tabs_w, bar.height()));
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(center_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            if ui
                .selectable_label(*module == Module::Library, "Library")
                .clicked()
            {
                *module = Module::Library;
            }
            if ui
                .selectable_label(*module == Module::Develop, "Develop")
                .clicked()
            {
                *module = Module::Develop;
            }
            if ui
                .selectable_label(*module == Module::Export, "Export")
                .clicked()
            {
                *module = Module::Export;
            }
        },
    );

    action
}
