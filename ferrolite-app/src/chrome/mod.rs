//! Custom window chrome: the borderless title bar, window controls, and app icon.
pub mod icon;
pub mod window_controls;

use crate::module::Module;
use crate::settings::keymap::{Action, Keymap};
use crate::widgets::TabRow;
use egui::{
    pos2, vec2, Align, Align2, Button, Color32, Context, FontId, Layout, PointerButton, Rect,
    Sense, Stroke, UiBuilder,
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
        pos2(bar.center().x + 110.0_f32, bar.bottom()),
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
            pos2(bar.center().x + 110.0_f32, bar.bottom()),
        );
        assert_eq!(center_rect.max.y, bar.bottom());
        assert_eq!(center_rect.height(), TITLEBAR_HEIGHT);
        assert_eq!(center_rect.center().x, bar.center().x);
        assert_eq!(center_rect.center().y, bar.center().y);
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
                .frame(egui::Frame::none())
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

    /// Regression test for the active-module accent underline (Spec 3.2 / UI v2).
    ///
    /// Runs the REAL production titlebar construction (identical to app.rs:
    /// `TopBottomPanel::top("titlebar").exact_height(30).frame(Frame::none().fill(BG_TITLEBAR))`
    /// → `title_bar` → `TabRow`) plus a `CentralPanel`, then inspects the frame's
    /// `FullOutput::shapes` and asserts the 2px `theme::ACCENT` underline is
    /// (a) painted, (b) FULLY inside its clip rect, and (c) not intersected by any
    /// shape painted after it.
    ///
    /// History: two earlier "fixes" moved the stroke relative to `rect.bottom()`
    /// while the tab rect silently overflowed the 30px bar (rect.bottom() == 34,
    /// clip bottom == 30), so the stroke was painted at y >= 30 and clipped to
    /// zero visible pixels. Asserting `clip.contains_rect(bbox)` here — not mere
    /// existence — is what catches that failure mode.
    #[test]
    fn titlebar_active_tab_underline_is_visible() {
        let ctx = Context::default();
        let mut module = Module::Export;
        let keymap = Keymap::default();

        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                pos2(0.0_f32, 0.0_f32),
                vec2(1280.0_f32, 800.0_f32),
            )),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            egui::TopBottomPanel::top("titlebar")
                .exact_height(TITLEBAR_HEIGHT)
                .frame(egui::Frame::none().fill(crate::theme::BG_TITLEBAR))
                .show(ctx, |ui| {
                    let _ = title_bar(
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
                });
            egui::CentralPanel::default().show(ctx, |_ui| {});
        });

        // Flatten the shape tree, preserving paint order.
        fn flatten(shape: &egui::Shape, clip: Rect, out: &mut Vec<(egui::Shape, Rect)>) {
            match shape {
                egui::Shape::Vec(shapes) => {
                    for s in shapes {
                        flatten(s, clip, out);
                    }
                }
                s => out.push((s.clone(), clip)),
            }
        }
        let mut flat: Vec<(egui::Shape, Rect)> = Vec::new();
        for clipped in &output.shapes {
            flatten(&clipped.shape, clipped.clip_rect, &mut flat);
        }

        let is_accent_underline = |shape: &egui::Shape| -> bool {
            matches!(
                shape,
                egui::Shape::LineSegment { points, stroke }
                    if stroke.color == egui::epaint::ColorMode::Solid(crate::theme::ACCENT)
                        && (stroke.width - 2.0_f32).abs() < 1e-4
                        && (points[0].y - points[1].y).abs() < 1e-4
            )
        };

        let (idx, (underline, clip)) = flat
            .iter()
            .enumerate()
            .find(|(_, (shape, _))| is_accent_underline(shape))
            .expect("active tab must paint a 2px horizontal theme::ACCENT underline");

        // Visible: the whole 2px stroke must lie inside its clip rect (the 30px
        // titlebar panel), not just intersect it.
        let bbox = underline.visual_bounding_rect();
        assert!(
            clip.contains_rect(bbox),
            "underline bbox {bbox:?} must be fully inside its clip rect {clip:?} \
             (a partially/fully clipped underline is invisible in the running app)"
        );
        assert!(
            bbox.max.y <= TITLEBAR_HEIGHT,
            "underline bbox {bbox:?} must lie within the {TITLEBAR_HEIGHT}px titlebar"
        );

        // Not covered: nothing painted after the underline may overlap it.
        for (shape, later_clip) in flat.iter().skip(idx + 1) {
            let later_bbox = shape.visual_bounding_rect();
            assert!(
                !(later_bbox.intersects(bbox) && later_clip.intersects(bbox)),
                "underline at {bbox:?} is painted over by a later shape: {shape:?}"
            );
        }
    }

    #[test]
    fn test_titlebar_single_border_and_tab_alignment() {
        let ctx = Context::default();
        let mut module = Module::Library;
        let keymap = Keymap::default();

        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::TopBottomPanel::top("titlebar_single_border")
                .exact_height(TITLEBAR_HEIGHT)
                .frame(egui::Frame::none())
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
                    let bar = ui.max_rect();
                    let center_rect = Rect::from_min_max(
                        pos2(bar.center().x - 110.0_f32, bar.top()),
                        pos2(bar.center().x + 110.0_f32, bar.bottom()),
                    );
                    assert_eq!(center_rect.center().y, bar.center().y);
                    assert_eq!(center_rect.height(), bar.height());
                    assert_eq!(center_rect.min.y, bar.top());
                    assert_eq!(center_rect.max.y, bar.bottom());
                });
        });
    }
}
