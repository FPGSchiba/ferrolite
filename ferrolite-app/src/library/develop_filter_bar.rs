//! Compact Develop filter strip: re-filter/sort the navigated set without leaving
//! Develop. Reuses `filter_widgets`; omits search + the metadata-range popover
//! (those stay in the Library toolbar).

use crate::library::filter_widgets as fw;
use crate::library::icons;
use crate::settings::keymap::Action;
use crate::state::AppState;

/// Draw the before/after split-compare toggle: a painter icon + label button
/// that lights up (accent background) while active, mirroring how
/// `selectable_label` shows selection. Returns true if it was clicked (caller
/// runs the shared `toggle_split_compare` path so `Y` / View menu / this
/// button all behave identically).
fn split_compare_button(
    ui: &mut egui::Ui,
    active: bool,
    keymap: &crate::settings::keymap::Keymap,
) -> bool {
    let text = "Before/After";
    let icon_size = 14.0;
    let gap = 6.0;
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(13.0),
        ui.visuals().text_color(),
    );
    let padding = egui::vec2(8.0, 4.0);
    let size = egui::vec2(
        icon_size + gap + galley.size().x + padding.x * 2.0,
        (icon_size).max(galley.size().y) + padding.y * 2.0,
    );
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        if active {
            ui.painter()
                .rect_filled(rect, 3.0, crate::theme::ACCENT_BG_SEL);
            ui.painter()
                .rect_stroke(rect, 3.0, egui::Stroke::new(1.0_f32, crate::theme::ACCENT));
        } else if hovered {
            ui.painter()
                .rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
        }
        let icon_color = if active {
            crate::theme::ACCENT_BRIGHT
        } else {
            ui.visuals().text_color()
        };
        let icon_center = egui::pos2(rect.left() + padding.x + icon_size * 0.5, rect.center().y);
        icons::split_compare(ui.painter(), icon_center, icon_size, icon_color);
        let text_pos = egui::pos2(
            rect.left() + padding.x + icon_size + gap,
            rect.center().y - galley.size().y * 0.5,
        );
        ui.painter().galley(text_pos, galley, icon_color);
    }

    resp.on_hover_text(format!(
        "Split-compare original vs current edit ({})",
        keymap.chord(Action::ToggleSplitCompare).label()
    ))
    .clicked()
}

/// What the filter bar did this frame: a filter/sort change (caller sets
/// `state.dirty` + persists it) and/or a request to toggle before/after split
/// (caller runs the shared `toggle_split_compare` path so this button behaves
/// identically to the `Y` shortcut and the View menu item).
pub struct FilterBarOutcome {
    pub changed: bool,
    pub toggle_split: bool,
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState) -> FilterBarOutcome {
    let mut changed = false;
    let mut toggle_split = false;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if let Some(v) = state.viewer.as_ref() {
            let active = v.split_compare;
            if split_compare_button(ui, active, &state.settings.keymap) {
                toggle_split = true;
            }
            ui.separator();
        }
        changed |= fw::sort_controls(ui, &mut state.filter.sort_key, &mut state.filter.sort_desc);
        changed |= fw::rating_threshold(
            ui,
            &mut state.filter.min_rating,
            &mut state.filter.rating_cmp,
        );
        changed |= fw::flag_filters(ui, &mut state.filter.flags);
        changed |= fw::tag_filter_dropdown(
            ui,
            &mut state.filter.tag_ids,
            &mut state.filter.tag_mode,
            &state.tags,
        );
    });
    FilterBarOutcome {
        changed,
        toggle_split,
    }
}
