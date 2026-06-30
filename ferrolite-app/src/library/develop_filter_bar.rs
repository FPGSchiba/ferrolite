//! Compact Develop filter strip: re-filter/sort the navigated set without leaving
//! Develop. Reuses `filter_widgets`; omits search + the metadata-range popover
//! (those stay in the Library toolbar).

use crate::library::filter_widgets as fw;
use crate::state::AppState;

/// Returns true if a filter/sort field changed (caller sets `state.dirty`).
pub fn show(ui: &mut egui::Ui, state: &mut AppState) -> bool {
    let mut changed = false;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
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
    changed
}
