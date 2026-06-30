//! Reusable right-click metadata menu for a single image (Rating / Flag / Tags /
//! Add-to-collection). Shared by the grid, the Develop filmstrip, and the loupe.

use crate::metadata::MetaEdit;
use crate::state::AppState;
use ferrolite_image::{Flag, Rating};

/// Render the menu for `image_id` inside a `context_menu` closure.
///
/// `single_image` controls scoping:
/// - `true` — always edit `image_id` only (Develop loupe and filmstrip).
/// - `false` — if `image_id` is in the current multi-selection, edit all selected
///   images; otherwise edit `image_id` only (grid).
pub fn show(ui: &mut egui::Ui, state: &mut AppState, image_id: i64, single_image: bool) {
    let ctx = ui.ctx().clone();
    // When single_image is true we always route through the single-image path,
    // ignoring whatever selection the grid may have left in state.
    let use_selection = !single_image && state.selection.contains(&image_id);
    let tags = state.tags.clone();
    let collections = state.collections.clone();
    let image_tags = state
        .visible_tags
        .get(&image_id)
        .cloned()
        .unwrap_or_default();

    // Helper: apply to the multi-selection or just this image depending on scope.
    let apply = |state: &mut AppState, edit: MetaEdit| {
        if use_selection {
            state.apply_metadata_edit(&ctx, edit);
        } else {
            state.apply_metadata_edit_to_image(&ctx, image_id, edit);
        }
    };

    ui.menu_button("Rating", |ui| {
        if ui.button("No rating").clicked() {
            apply(state, MetaEdit::SetRating(Rating::new(0)));
            ui.close_menu();
        }
        for n in 1u8..=5 {
            let label = format!("{n} star{}", if n == 1 { "" } else { "s" });
            if ui.button(label).clicked() {
                apply(state, MetaEdit::SetRating(Rating::new(n)));
                ui.close_menu();
            }
        }
    });
    ui.menu_button("Flag", |ui| {
        if ui.button("Pick").clicked() {
            apply(state, MetaEdit::SetFlag(Flag::Pick));
            ui.close_menu();
        }
        if ui.button("Reject").clicked() {
            apply(state, MetaEdit::SetFlag(Flag::Reject));
            ui.close_menu();
        }
        if ui.button("Unflag").clicked() {
            apply(state, MetaEdit::SetFlag(Flag::None));
            ui.close_menu();
        }
    });
    if !tags.is_empty() {
        ui.menu_button("Tags", |ui| {
            for t in &tags {
                let has = image_tags.contains(&t.id);
                if ui.selectable_label(has, &t.name).clicked() {
                    apply(state, MetaEdit::ToggleTag(t.id));
                    ui.close_menu();
                }
            }
        });
    }
    if !collections.is_empty() {
        ui.menu_button("Add to collection", |ui| {
            for c in &collections {
                if ui.button(&c.name).clicked() {
                    if use_selection {
                        state.add_selection_to_collection(c.id);
                    } else {
                        state.add_image_to_collection_now(image_id, c.id);
                    }
                    ui.close_menu();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    /// `single_image = true` must never activate the multi-select path, even when
    /// `image_id` is present in a stale grid multi-selection.  Mirrors the
    /// `use_selection` computation inside `show`.
    #[test]
    fn single_image_ignores_selection() {
        let image_id: i64 = 42;
        let mut selection = std::collections::HashSet::new();
        selection.insert(image_id); // stale multi-select

        // Mirrors the formula in `show`: use_selection = !single_image && selection.contains(&id)
        let compute_use_selection =
            |single_image: bool| -> bool { !single_image && selection.contains(&image_id) };

        assert!(
            !compute_use_selection(true),
            "single_image=true must not use the selection even when image_id is selected"
        );
        assert!(
            compute_use_selection(false),
            "single_image=false with image_id in selection should use multi-select path"
        );
    }
}
