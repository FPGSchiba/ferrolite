//! The Help modal: an About blurb plus a live keyboard-shortcut reference
//! generated from the current `Keymap` (so a rebind in Settings is reflected
//! here automatically). Mirrors the remove-confirmation modal's styling
//! (`app.rs`, `pending_remove`): a centered, non-resizable `egui::Window`
//! with Esc/Close-button dismissal.

use crate::settings::keymap::{Action, Keymap};
use crate::theme;

/// One shortcut-list group: a heading plus the actions shown under it, in
/// the order they should render. Kept as a flat static table so adding a
/// new `Action` only requires adding it here (and to `Action::ALL`).
const GROUPS: &[(&str, &[Action])] = &[
    (
        "Navigation",
        &[
            Action::CloseViewer,
            Action::OpenImage,
            Action::SelectAll,
            Action::PrevImage,
            Action::NextImage,
        ],
    ),
    (
        "Rating & Flags",
        &[
            Action::Rating0,
            Action::Rating1,
            Action::Rating2,
            Action::Rating3,
            Action::Rating4,
            Action::Rating5,
            Action::FlagPick,
            Action::FlagReject,
        ],
    ),
    (
        "Develop",
        &[
            Action::HoldBeforePeek,
            Action::ToggleSplitCompare,
            Action::AddToQueue,
            Action::SwitchToolAdjust,
            Action::SwitchToolCrop,
            Action::SwitchToolMask,
            Action::ToggleMaskOverlay,
        ],
    ),
    ("Editing", &[Action::Undo, Action::Redo]),
    ("App", &[Action::OpenSettings, Action::OpenHelp]),
];

/// Render the Help modal if `*open`. Closes (`*open = false`) on the Close
/// button, Esc, or a click on the dimmed backdrop.
pub fn show(ctx: &egui::Context, open: &mut bool, keymap: &Keymap) {
    if !*open {
        return;
    }

    let mut still_open = true;

    // Dimmed backdrop, consistent with other modal overlays in the app —
    // drawn in `Order::Middle` (below the `Order::Foreground` window added
    // below) so the window content stays on top and clickable. The backdrop
    // area is given a click `Sense` so it captures pointer input rather than
    // letting it fall through to the app underneath, and a click on it
    // (i.e. anywhere outside the Help window) closes the modal.
    egui::Area::new(egui::Id::new("help_modal_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(egui::Pos2::ZERO)
        .show(ctx, |ui| {
            let screen = ctx.screen_rect();
            ui.painter()
                .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(140));
            let response = ui.interact(
                screen,
                ui.id().with("backdrop_click_catcher"),
                egui::Sense::click(),
            );
            if response.clicked() {
                still_open = false;
            }
        });

    egui::Window::new("Help")
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .fixed_size(egui::vec2(480.0, 520.0))
        .frame(egui::Frame::window(&ctx.style()).inner_margin(egui::Margin::symmetric(18.0, 12.0)))
        .show(ctx, |ui| {
            // Reserve room on the right, inside the scroll area, so the
            // floating vertical scrollbar (drawn as an overlay on the right
            // edge of the scroll area's content) never overlaps the
            // right-aligned keybind chords drawn by `draw_shortcuts`. The
            // window frame above already gives a symmetric 18px left/right
            // margin; this inner margin keeps that same visual balance once
            // the scrollbar claims space on the right.
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 0.0,
                        right: 16.0,
                        top: 0.0,
                        bottom: 0.0,
                    })
                    .show(ui, |ui| {
                        draw_about(ui);
                        ui.add_space(12.0);
                        ui.separator();
                        ui.add_space(12.0);
                        draw_shortcuts(ui, keymap);
                    });
            });

            ui.add_space(12.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Close").clicked() {
                    still_open = false;
                }
            });
        });

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        still_open = false;
    }

    *open = still_open;
}

fn draw_about(ui: &mut egui::Ui) {
    ui.heading("Ferrolite v0.0.1");
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("A fast, GPU-accelerated RAW photo cataloguing and editing app.")
            .color(theme::TEXT_DIM),
    );
    ui.label(
        egui::RichText::new("License: GPL-3.0")
            .color(theme::TEXT_DIM)
            .size(11.0),
    );
}

fn draw_shortcuts(ui: &mut egui::Ui, keymap: &Keymap) {
    ui.heading("Keyboard shortcuts");
    ui.add_space(6.0);

    for (group_name, actions) in GROUPS {
        ui.label(
            egui::RichText::new(*group_name)
                .strong()
                .color(theme::ACCENT),
        );
        ui.add_space(2.0);
        egui::Grid::new(("help_shortcut_grid", *group_name))
            .num_columns(2)
            .spacing(egui::vec2(24.0, 4.0))
            .striped(false)
            .show(ui, |ui| {
                for action in *actions {
                    ui.label(action.label());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(keymap.chord(*action).label())
                                .monospace()
                                .color(theme::TEXT_PRIMARY),
                        );
                    });
                    ui.end_row();
                }
                // Not a rebindable `Action` (it's a scroll gesture, not a
                // chord), so it's a manually-drawn row rather than part of
                // `GROUPS`. Documents the Mask > Brush size gesture.
                if *group_name == "Develop" {
                    ui.label("Brush size (Mask > Brush)");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("Ctrl + scroll")
                                .monospace()
                                .color(theme::TEXT_PRIMARY),
                        );
                    });
                    ui.end_row();
                }
            });
        ui.add_space(10.0);
    }
}
