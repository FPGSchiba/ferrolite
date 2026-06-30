//! Reusable right-click metadata menu for a single image (Rating / Flag / Tags /
//! Add-to-collection). Shared by the grid, the Develop filmstrip, and the loupe.

use crate::metadata::MetaEdit;
use crate::state::AppState;
use ferrolite_image::{Flag, Rating};

/// Render the menu for `image_id` inside a `context_menu` closure. Scopes edits
/// to this image when it is not part of the current multi-selection.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, image_id: i64) {
    let ctx = ui.ctx().clone();
    let in_selection = state.selection.contains(&image_id);
    let tags = state.tags.clone();
    let collections = state.collections.clone();
    let image_tags = state
        .visible_tags
        .get(&image_id)
        .cloned()
        .unwrap_or_default();

    // Helper: apply to the multi-selection if this image is in it, else just this image.
    let apply = |state: &mut AppState, edit: MetaEdit| {
        if in_selection {
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
                    if in_selection {
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
