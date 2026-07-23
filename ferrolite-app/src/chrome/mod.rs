//! Custom window chrome: the borderless title bar, window controls, and app icon.
pub mod icon;
pub mod window_controls;

use crate::module::Module;
use crate::settings::keymap::{Action, Keymap};
use crate::widgets::TabRow;
use egui::{
    pos2, vec2, Align, Align2, Button, Color32, Context, FontId, Layout, PointerButton, Rect,
    Rounding, Sense, Stroke, UiBuilder,
};

/// Titlebar layout constants (Spec 3.2).
#[allow(dead_code)]
pub const TITLEBAR_HEIGHT: f32 = 30.0_f32;
#[allow(dead_code)]
pub const TITLEBAR_BG: Color32 = Color32::from_rgb(0x11, 0x11, 0x11);
#[allow(dead_code)]
pub const TITLEBAR_BORDER: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);
#[allow(dead_code)]
pub const VERSION_STRING: &str = "v0.1.2";

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
    ToggleInfoOverlay,
    ToggleToolPalette,
    OpenHelp,
    OpenSettings,
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
/// Left: icon + wordmark + interactive menu row. Center: Library/Develop/Export
/// tabs rendered with `TabRow`. Right: window controls + version. Empty space drags the window.
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
    show_info_overlay: bool,
    show_tool_palette: bool,
) -> Option<MenuAction> {
    let bar = ui.max_rect();

    // 1. Paint background #111111 and 1px #262626 bottom border.
    ui.painter().rect_filled(bar, Rounding::ZERO, TITLEBAR_BG);
    ui.painter().line_segment(
        [
            pos2(bar.left(), bar.bottom() - 0.5_f32),
            pos2(bar.right(), bar.bottom() - 0.5_f32),
        ],
        Stroke::new(1.0_f32, TITLEBAR_BORDER),
    );

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
        let mut x = bar.left() + 8.0_f32;

        // Logo mark: 14×14 accent square filled with letter "F".
        icon::paint_mark(
            painter,
            Rect::from_min_size(pos2(x, cy - 7.0_f32), vec2(14.0_f32, 14.0_f32)),
        );
        x += 20.0_f32;

        // Header text "FERROLITE" (11px, bold #dcdcdc)
        let logo = painter.text(
            pos2(x, cy),
            Align2::LEFT_CENTER,
            "FERROLITE",
            FontId::proportional(11.0_f32),
            Color32::from_rgb(0xdc, 0xdc, 0xdc),
        );
        logo.right() + 14.0_f32
    };

    // Interactive menu row (on top of the drag region).
    let mut action: Option<MenuAction> = None;
    let menu_rect = Rect::from_min_max(
        pos2(x, bar.top()),
        pos2(bar.center().x - 110.0_f32, bar.bottom()),
    );
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(menu_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 12.0_f32;
            ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::NONE;
            ui.visuals_mut().widgets.inactive.fg_stroke =
                Stroke::new(1.0_f32, Color32::from_rgb(0x9a, 0x9a, 0x9a));

            ui.menu_button("File", |ui| {
                if menu_button(ui, keymap, "Settings…", Action::OpenSettings, true).clicked() {
                    action = Some(MenuAction::OpenSettings);
                    ui.close_menu();
                }
                ui.separator();
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
                if menu_button(ui, keymap, "Fit", Action::ZoomFit, viewer_open).clicked() {
                    action = Some(MenuAction::ZoomFit);
                    ui.close_menu();
                }
                if menu_button(ui, keymap, "1:1", Action::ZoomActual, viewer_open).clicked() {
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
                let mut info_overlay_checked = show_info_overlay;
                if ui
                    .checkbox(
                        &mut info_overlay_checked,
                        format!("{} Show info overlay", crate::icons::INFO),
                    )
                    .clicked()
                {
                    action = Some(MenuAction::ToggleInfoOverlay);
                    ui.close_menu();
                }
                let mut palette_checked = show_tool_palette;
                if ui
                    .checkbox(&mut palette_checked, "Show tool palette")
                    .clicked()
                {
                    action = Some(MenuAction::ToggleToolPalette);
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

    // Right group: window controls + version string "v0.1.2" (IBM Plex Mono, 10.5px, #6a6a6a).
    let control_clicked = ui
        .allocate_new_ui(
            UiBuilder::new()
                .max_rect(bar)
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 0.0_f32;
                let clicked = window_controls::controls_ui(ui, is_maximized);
                ui.add_space(8.0_f32);
                ui.add(egui::Label::new(
                    egui::RichText::new(version)
                        .font(FontId::monospace(10.5_f32))
                        .color(Color32::from_rgb(0x6a, 0x6a, 0x6a)),
                ));
                clicked
            },
        )
        .inner;
    if let Some(action) = control_clicked {
        ctx.send_viewport_cmd(window_controls::command(action, is_maximized));
    }

    // Center-right group: Library / Develop / Export navigation tabs rendered using `TabRow`.
    let tabs = [
        (Module::Library, "Library"),
        (Module::Develop, "Develop"),
        (Module::Export, "Export"),
    ];
    let center_rect = Rect::from_min_max(
        pos2(bar.center().x - 110.0_f32, bar.top()),
        pos2(bar.center().x + 110.0_f32, bar.bottom() - 2.0_f32),
    );
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(center_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            TabRow::new(module, &tabs).ui(ui);
        },
    );

    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::RawInput;

    #[test]
    fn titlebar_layout_constants() {
        assert_eq!(TITLEBAR_HEIGHT, 30.0_f32);
        assert_eq!(TITLEBAR_BG, Color32::from_rgb(0x11, 0x11, 0x11));
        assert_eq!(TITLEBAR_BORDER, Color32::from_rgb(0x26, 0x26, 0x26));
    }

    #[test]
    fn titlebar_version_string() {
        assert_eq!(VERSION_STRING, "v0.1.2");
    }

    #[test]
    fn titlebar_tab_bounds_alignment() {
        let bar = Rect::from_min_size(pos2(0.0_f32, 0.0_f32), vec2(1000.0_f32, TITLEBAR_HEIGHT));
        let center_rect = Rect::from_min_max(
            pos2(bar.center().x - 110.0_f32, bar.top()),
            pos2(bar.center().x + 110.0_f32, bar.bottom() - 2.0_f32),
        );
        assert_eq!(center_rect.max.y, bar.bottom() - 2.0_f32);
        assert_eq!(center_rect.height(), TITLEBAR_HEIGHT - 2.0_f32);
        assert_eq!(center_rect.center().x, bar.center().x);
        assert_eq!(center_rect.width(), 220.0_f32);
    }

    #[test]
    fn titlebar_renders_without_panic() {
        let ctx = Context::default();
        let mut module = Module::Library;
        let keymap = Keymap::default();

        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::TopBottomPanel::top("titlebar_test")
                .exact_height(TITLEBAR_HEIGHT)
                .show(ctx, |ui| {
                    let action = title_bar(
                        ctx,
                        ui,
                        &mut module,
                        VERSION_STRING,
                        false,
                        false,
                        &keymap,
                        false,
                        false,
                        false,
                        false,
                        false,
                    );
                    assert!(action.is_none());
                });
        });

        assert_eq!(module, Module::Library);
    }
}
