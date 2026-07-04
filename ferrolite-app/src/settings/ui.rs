//! The Settings window: a modal tabbed dialog (General · Keyboard) editing
//! `crate::settings::Settings` in place. Mirrors the Help modal's backdrop +
//! Esc/Close dismissal (`ferrolite-app/src/help.rs`) for visual consistency.
//!
//! The keyboard-rebinding tab lives in `ui/keyboard.rs` to keep this file
//! focused; both are joined here into a single `pub fn show` entry point.

mod keyboard;

use super::{dto, Settings};
use crate::theme;

/// Which tab is active, stored in `egui` memory so it survives across frames
/// without needing a field on `FerroliteApp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SettingsTab {
    #[default]
    General,
    Keyboard,
}

fn tab_id() -> egui::Id {
    egui::Id::new("settings_active_tab")
}

fn active_tab(ctx: &egui::Context) -> SettingsTab {
    ctx.data(|d| d.get_temp(tab_id())).unwrap_or_default()
}

fn set_active_tab(ctx: &egui::Context, tab: SettingsTab) {
    ctx.data_mut(|d| d.insert_temp(tab_id(), tab));
}

/// Render the Settings modal if `*open`. Returns `true` if any setting was
/// changed this frame (the caller should then persist + apply as needed).
/// Closes (`*open = false`) on the Close button, Esc, or a backdrop click.
pub fn show(
    ctx: &egui::Context,
    open: &mut bool,
    settings: &mut Settings,
    display_name: &str,
) -> bool {
    if !*open {
        return false;
    }

    let mut still_open = true;
    let mut changed = false;

    // Dimmed backdrop — same pattern as `help::show`: `Order::Middle`, click
    // to close, so the window content (added below at `Order::Foreground`)
    // stays on top and clickable while everything underneath is blocked.
    egui::Area::new(egui::Id::new("settings_modal_backdrop"))
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

    // Fixed content width, matching the Help modal's sized/centered approach
    // (`help.rs`'s `fixed_size`) so this renders as a proper dialog rather
    // than a full-width band. Height is capped but not fixed — `max_height`
    // lets short content (e.g. the General tab) size down while the
    // Keyboard tab's longer row list scrolls internally instead of growing
    // the window past the cap.
    const CONTENT_WIDTH: f32 = 600.0;
    const MAX_HEIGHT: f32 = 560.0;

    egui::Window::new("Settings")
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .max_width(CONTENT_WIDTH)
        .max_height(MAX_HEIGHT)
        .show(ctx, |ui| {
            // Lock the content `Ui` to a fixed width so no child widget
            // (combo box, slider, grid) can force the window wider — this is
            // what keeps the window a sized dialog instead of a full-width
            // band when content wants more horizontal room.
            ui.set_width(CONTENT_WIDTH);

            let mut tab = active_tab(ctx);
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(tab == SettingsTab::General, "General")
                    .clicked()
                {
                    tab = SettingsTab::General;
                }
                if ui
                    .selectable_label(tab == SettingsTab::Keyboard, "Keyboard")
                    .clicked()
                {
                    tab = SettingsTab::Keyboard;
                }
            });
            set_active_tab(ctx, tab);

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);

            egui::ScrollArea::vertical()
                .id_salt("settings_content_scroll")
                .max_height(MAX_HEIGHT - 96.0)
                .show(ui, |ui| {
                    ui.set_width(CONTENT_WIDTH);
                    match tab {
                        SettingsTab::General => {
                            if draw_general_tab(ui, settings, display_name) {
                                changed = true;
                            }
                        }
                        SettingsTab::Keyboard => {
                            if keyboard::draw(ui, settings) {
                                changed = true;
                            }
                        }
                    }
                });

            ui.add_space(8.0);
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
    changed
}

/// General preferences tab: session restore, remove confirmation, histogram
/// overlay, default working space, default grid (thumbnail) size, and the
/// display (monitor color) profile mode.
fn draw_general_tab(ui: &mut egui::Ui, settings: &mut Settings, display_name: &str) -> bool {
    let mut changed = false;

    ui.heading("General");
    ui.add_space(8.0);

    if ui
        .checkbox(
            &mut settings.restore_session,
            "Restore last session on startup",
        )
        .changed()
    {
        changed = true;
    }
    if ui
        .checkbox(
            &mut settings.confirm_remove,
            "Confirm before removing images",
        )
        .changed()
    {
        changed = true;
    }
    if ui
        .checkbox(&mut settings.show_histogram, "Show histogram overlay")
        .changed()
    {
        changed = true;
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(12.0);

    ui.label("Default working space (applied at startup)");
    ui.add_space(4.0);
    let mut ws = settings.working_space.to_ws();
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("settings_working_space")
            .selected_text(format!("{ws:?}"))
            .show_ui(ui, |ui| {
                for w in ferrolite_color::WorkingSpace::ALL {
                    ui.selectable_value(&mut ws, w, format!("{w:?}"));
                }
            });
    });
    if ws != settings.working_space.to_ws() {
        settings.working_space = super::dto::PersistedWorkingSpace::from_ws(ws);
        changed = true;
    }

    ui.add_space(12.0);

    // Same range as the Library toolbar's thumbnail-size slider
    // (`library/toolbar.rs`'s `EguiSlider { min: 0.0, max: 100.0, .. }`), so
    // this default matches what the in-grid slider can actually reach.
    ui.label("Default thumbnail size");
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui
            .add(egui::Slider::new(&mut settings.grid_size, 0.0..=100.0))
            .changed()
        {
            changed = true;
        }
    });

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(12.0);

    // Display (monitor color) group. Config-surface preference, not an
    // editing control — deliberately no per-control reset arrow (see
    // CLAUDE.md's per-component reset rule, which scopes to editable
    // controls in the Develop module, not Settings preferences); the
    // "reset" here is simply choosing Auto.
    ui.heading("Display");
    ui.add_space(8.0);
    ui.label(format!("Active profile: {display_name}"));
    ui.add_space(4.0);
    let mut mode = settings.display_profile.clone();
    let mut display_changed = false;
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Monitor color")
            .selected_text(match &mode {
                dto::PersistedDisplayProfile::Auto => "Auto (detect)".to_string(),
                dto::PersistedDisplayProfile::Srgb => "sRGB".to_string(),
                dto::PersistedDisplayProfile::Custom(_) => "Custom file…".to_string(),
            })
            .show_ui(ui, |ui| {
                display_changed |= ui
                    .selectable_value(
                        &mut mode,
                        dto::PersistedDisplayProfile::Auto,
                        "Auto (detect)",
                    )
                    .changed();
                display_changed |= ui
                    .selectable_value(&mut mode, dto::PersistedDisplayProfile::Srgb, "sRGB")
                    .changed();
                if ui
                    .selectable_label(
                        matches!(mode, dto::PersistedDisplayProfile::Custom(_)),
                        "Custom file…",
                    )
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("ICC profile", &["icc", "icm"])
                        .pick_file()
                    {
                        mode = dto::PersistedDisplayProfile::Custom(path);
                        display_changed = true;
                    }
                }
            });
        if ui.button("Redetect").clicked() {
            // Force a re-resolve even if the mode itself is unchanged (e.g.
            // the user moved the window to another monitor and wants to
            // re-check without touching the mode).
            display_changed = true;
        }
    });
    if display_changed {
        settings.display_profile = mode;
        changed = true;
    }

    ui.add_space(16.0);
    ui.label(
        egui::RichText::new("Export settings and the Library filter are saved automatically.")
            .color(theme::TEXT_DIM)
            .size(11.0),
    );

    changed
}
